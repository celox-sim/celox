//! Variables and memories that several processes of a module write.

use super::*;

/// Memories may have several edge-sensitive writers. Their NBA stores update
/// selected elements; always_ff additionally requires a single writing process.
///
/// A memory that repeating processes of other modules write through
/// hierarchical names (`foreign_writes`) has writers outside the module, so
/// its edge writes update selected elements too.
pub(super) fn shared_edge_memories(
    module: &sv::ir::Module,
    drivers: &ModuleDrivers,
    foreign_writes: &HashMap<String, hierarchy::ForeignWrites>,
    warnings: &mut Vec<crate::warning::PendingWarning>,
) -> HashSet<String> {
    let mut memories = HashSet::default();
    let first_ff = module.comb_processes().len();
    let end_ff = first_ff + module.ff_processes().len();
    for signal in module
        .signals()
        .iter()
        .filter(|s| !s.is_net() && !s.r#type().unpacked_ranges().is_empty())
    {
        let drivers = drivers.local(signal.name());
        if drivers
            .iter()
            .any(|(id, _)| !(first_ff..end_ff).contains(id))
        {
            continue;
        }
        if foreign_writes
            .get(signal.name())
            .is_some_and(|writes| writes.repeating > 0)
        {
            if !drivers.is_empty() {
                memories.insert(signal.name().into());
            }
            continue;
        }
        let overlaps = drivers.iter().enumerate().any(|(i, a)| {
            drivers[i + 1..]
                .iter()
                .any(|b| a.0 != b.0 && net_driver_ranges_overlap(a.1, b.1))
        });
        if !overlaps {
            continue;
        }
        let mut restricted = false;
        for (id, _) in drivers {
            let process = &module.ff_processes()[id - first_ff];
            restricted |= process.is_always_ff();
        }
        if restricted {
            warnings.push((
                crate::FrontendWarningKind::MultipleAlwaysFfDrivers {
                    module: module.name().into(),
                    signal: signal.name().into(),
                },
                None,
            ));
        }
        memories.insert(signal.name().into());
    }
    memories
}

/// `module` with its `always @*` blocks that write a common variable
/// rearranged, and the variables they write together as processes, or
/// `None` when no blocks write a common variable.
///
/// Several plain `always` processes may write one variable (IEEE 1800-2023
/// 6.5, 9.2.2), such as a loop index, and each block waits on the variables
/// it reads, a shared one included (9.4.2.2). So the blocks run as
/// processes, whose writes notify other blocks that are waiting. For isolated
/// indices written before reads, let other Active work run after the body but
/// before its next implicit wait, as 4.7 and 4.9.3 permit. The shared variable's
/// writes and event notifications remain; no private copy is substituted.
/// Blocks that still keep waking each other report the time and processes.
///
/// With `shared.temporaries`, a variable becomes a temporary of each block
/// instead when only such blocks write it, each writes it before it reads
/// it on every path, and nothing else reads it: no other process,
/// continuous assignment, subroutine, port, instance connection or
/// hierarchical name. Its writes then wake no other block, unlike in the
/// standard, and the variable itself keeps its initial value.
pub(super) fn share_always_variables(
    module: &sv::ir::Module,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    shared: SharedAlways<'_>,
    warnings: &mut Vec<crate::warning::PendingWarning>,
) -> Option<(sv::ir::Module, HashSet<String>)> {
    let comb = module.comb_processes();
    let mut shared_names = HashSet::default();
    let drivers = ModuleDrivers::new(module, constants, parameter_types);
    let mut processes = HashSet::default();
    let mut deferred_rearm = HashSet::default();
    let mut temporaries: Vec<(&sv::ir::Signal, Vec<usize>)> = Vec::new();
    for signal in module.signals().iter().filter(|signal| !signal.is_net()) {
        let mut writers: Vec<usize> = drivers
            .local(signal.name())
            .iter()
            .map(|(driver, _)| *driver)
            .collect();
        writers.sort_unstable();
        writers.dedup();
        if writers.len() < 2
            || !writers.iter().all(|&driver| {
                comb.get(driver)
                    .is_some_and(|process| process.kind() == sv::ir::CombProcessKind::AlwaysStar)
            })
        {
            continue;
        }
        let isolated = !shared.foreign_references.contains(signal.name())
            && !read_outside(module, signal.name(), &writers)
            && writers.iter().all(|&driver| {
                let body = comb[driver].body();
                !has_nonblocking_write(body, signal.name())
                    && written_before_read(body, signal.name(), false) == Some(true)
            });
        if shared.temporaries && isolated {
            temporaries.push((signal, writers));
        } else {
            if isolated {
                deferred_rearm.extend(writers.iter().copied());
            }
            processes.extend(writers);
            shared_names.insert(signal.name().to_string());
        }
    }
    if processes.is_empty() && temporaries.is_empty() {
        return None;
    }
    let mut module = module.clone();
    let mut renames: HashMap<usize, HashMap<String, String>> = HashMap::default();
    for (signal, writers) in &temporaries {
        let mut sites: Vec<String> = writers
            .iter()
            .filter_map(|&driver| driver_site(&module, driver))
            .collect();
        sites.sort_unstable();
        warnings.push((
            crate::FrontendWarningKind::SharedAlwaysTemporary {
                module: module.name().into(),
                signal: signal.name().into(),
                blocks: sites.join(", "),
            },
            comb[writers[0]].site().cloned(),
        ));
        for &driver in writers {
            let local = format!("{}$always{driver}", signal.name());
            module = module.add_local(sv::ir::LocalVariable {
                name: local.clone(),
                source_name: signal.name().into(),
                r#type: signal.r#type().clone(),
                owned: false,
            });
            renames
                .entry(driver)
                .or_default()
                .insert(signal.name().into(), local);
        }
    }
    let module = module.map_comb_process_bodies(|index, body, sensitivity| {
        let Some(renames) = renames.get(&index) else {
            return body;
        };
        let mut renamed: Vec<sv::ir::Stmt> = Vec::new();
        if !sensitivity {
            // Each run starts a new temporary.
            let mut locals: Vec<&String> = renames.values().collect();
            locals.sort_unstable();
            renamed.extend(locals.into_iter().map(|name| sv::ir::Stmt::Local {
                name: name.clone(),
                init: None,
            }));
        }
        renamed.extend(body.into_iter().map(|stmt| {
            stmt.map(
                &mut |expr| ff::rename_idents(&expr, renames),
                &mut |lvalue| rename_lvalue(lvalue, renames),
            )
        }));
        renamed
    });
    Some((
        module.run_comb_processes_with_deferred_rearm(|index, _| {
            processes
                .contains(&index)
                .then_some(deferred_rearm.contains(&index))
        }),
        shared_names,
    ))
}

/// `lvalue` with the identifiers in `renames` replaced.
fn rename_lvalue(lvalue: sv::ir::LValue, renames: &HashMap<String, String>) -> sv::ir::LValue {
    match lvalue {
        sv::ir::LValue::Ident(name) => {
            sv::ir::LValue::Ident(renames.get(&name).cloned().unwrap_or(name))
        }
        sv::ir::LValue::Select {
            name,
            msb,
            lsb,
            array_slice_width,
            array_slice_reversed,
            window,
        } => sv::ir::LValue::Select {
            name: renames.get(&name).cloned().unwrap_or(name),
            msb: ff::rename_const_idents(&msb, renames),
            lsb: ff::rename_const_idents(&lsb, renames),
            array_slice_width: array_slice_width
                .map(|width| Box::new(ff::rename_const_idents(&width, renames))),
            array_slice_reversed,
            window,
        },
    }
}

/// Whether `stmts` mention `name`, read or written.
fn stmts_mention(stmts: &[sv::ir::Stmt], name: &str) -> bool {
    let renames: HashMap<String, String> =
        std::iter::once((name.to_string(), format!("{name}$"))).collect();
    stmts.iter().any(|stmt| {
        let mut expression_found = false;
        let mut target_found = false;
        stmt.clone().map(
            &mut |expr| {
                expression_found |= ff::rename_idents(&expr, &renames) != expr;
                expr
            },
            &mut |lvalue| {
                target_found |= rename_lvalue(lvalue.clone(), &renames) != lvalue;
                lvalue
            },
        );
        expression_found || target_found
    })
}

/// Whether something other than the processes `writers` (indices among the
/// combinational processes) of `module` reads or writes `name`.
fn read_outside(module: &sv::ir::Module, name: &str, writers: &[usize]) -> bool {
    let renames: HashMap<String, String> =
        std::iter::once((name.to_string(), format!("{name}$"))).collect();
    let mentions = |expr: &sv::ir::Expr| ff::rename_idents(expr, &renames) != *expr;
    module.ports().iter().any(|port| port.name() == name)
        || module
            .comb_processes()
            .iter()
            .enumerate()
            .filter(|(index, _)| !writers.contains(index))
            .any(|(_, process)| {
                stmts_mention(process.body(), name)
                    || stmts_mention(process.sensitivity_body(), name)
            })
        || module
            .ff_processes()
            .iter()
            .any(|process| stmts_mention(process.body(), name))
        || module
            .initial_processes()
            .iter()
            .any(|process| stmts_mention(process.body(), name))
        || module
            .subroutines()
            .iter()
            .any(|subroutine| stmts_mention(&subroutine.body, name))
        || module
            .assignments()
            .iter()
            .any(|assignment| assignment.lhs() == name || mentions(assignment.rhs()))
        || module.instances().iter().any(|instance| {
            instance.port_connections().iter().any(|connection| {
                connection.actual() == name || connection.actual_expr().is_some_and(mentions)
            })
        })
}

/// A queued write cannot initialize a temporary or remain combinational
/// after privatization, even if a blocking assignment precedes it.
fn has_nonblocking_write(stmts: &[sv::ir::Stmt], name: &str) -> bool {
    use sv::ir::Stmt;
    stmts.iter().any(|stmt| {
        let mut found = false;
        stmt.walk(&mut |stmt| {
            found |= match stmt {
                Stmt::Assign {
                    lhs,
                    nonblocking: true,
                    ..
                } => lhs.name() == name,
                Stmt::AssignConcat {
                    parts,
                    nonblocking: true,
                    ..
                } => parts.iter().any(|part| part.name() == name),
                _ => false,
            };
        });
        found
    })
}

/// Whether every path through `stmts` writes the whole of `name` before it
/// reads it: `Some` with whether `name` is written when they end, or `None`
/// when a read may come before the write. `assigned` says whether it is
/// written when they start.
fn written_before_read(stmts: &[sv::ir::Stmt], name: &str, mut assigned: bool) -> Option<bool> {
    use sv::ir::Stmt;
    let reads = |stmt: &Stmt| stmts_mention(std::slice::from_ref(stmt), name);
    for stmt in stmts {
        assigned = match stmt {
            Stmt::Assign {
                lhs,
                rhs,
                nonblocking: false,
            } => {
                let whole = matches!(lhs, sv::ir::LValue::Ident(target) if target == name);
                let probe = Stmt::Eval(rhs.clone());
                let index_reads = !whole
                    && matches!(lhs, sv::ir::LValue::Select { .. })
                    && lhs.name() != name
                    && reads(stmt);
                if !assigned && (reads(&probe) || index_reads) {
                    return None;
                }
                assigned || whole
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                if !assigned && reads(&Stmt::Eval(condition.clone())) {
                    return None;
                }
                let then_assigned = written_before_read(then_body, name, assigned)?;
                let else_assigned = written_before_read(else_body, name, assigned)?;
                then_assigned && else_assigned
            }
            Stmt::Loop {
                kind,
                init,
                condition,
                step,
                body,
            } => {
                let initialized = written_before_read(init, name, assigned)?;
                let header = Stmt::Loop {
                    kind: kind.clone(),
                    init: Vec::new(),
                    condition: condition.clone(),
                    step: Vec::new(),
                    body: Vec::new(),
                };
                if !initialized && reads(&header) {
                    return None;
                }
                // The body and the step may not run.
                let after_body = written_before_read(body, name, initialized)?;
                written_before_read(step, name, after_body)?;
                initialized
            }
            Stmt::Break | Stmt::Continue => assigned,
            // Other statements, case statements included, only read here.
            _ => {
                if !assigned && reads(stmt) {
                    return None;
                }
                assigned
            }
        };
    }
    Some(assigned)
}

/// Variables, other than unpacked arrays, that several plain edge-triggered
/// `always` processes write only by blocking assignments, such as a loop
/// index they share. A variable may have several procedural writers, and the
/// last write determines its value (IEEE 1800-2023 6.5); only always_comb,
/// always_latch and always_ff restrict it to one process (9.2.2.2, 9.2.2.4).
/// The processes read and write the variable's stable state directly (see
/// `Ff::with_shared_variables`).
pub(super) fn shared_edge_variables(
    module: &sv::ir::Module,
    drivers: &ModuleDrivers,
) -> HashSet<String> {
    let first_ff = module.comb_processes().len();
    let end_ff = first_ff + module.ff_processes().len();
    let mut nonblocking = HashSet::default();
    for process in module.ff_processes() {
        for stmt in process.body() {
            stmt.walk(&mut |stmt| match stmt {
                sv::ir::Stmt::Assign {
                    lhs,
                    nonblocking: true,
                    ..
                } => {
                    nonblocking.insert(lhs.name());
                }
                sv::ir::Stmt::AssignConcat {
                    parts,
                    nonblocking: true,
                    ..
                } => nonblocking.extend(parts.iter().map(sv::ir::LValue::name)),
                _ => {}
            });
        }
    }
    module
        .signals()
        .iter()
        .filter(|signal| {
            !signal.is_net()
                && signal.r#type().unpacked_ranges().is_empty()
                && !nonblocking.contains(signal.name())
        })
        .filter(|signal| {
            let drivers = drivers.local(signal.name());
            drivers.iter().any(|(id, _)| drivers[0].0 != *id)
                && drivers.iter().all(|(id, _)| {
                    (first_ff..end_ff).contains(id)
                        && !module.ff_processes()[id - first_ff].is_always_ff()
                })
        })
        .map(|signal| signal.name().to_string())
        .collect()
}

/// The unpacked arrays that only edge-triggered processes of `module` drive.
/// A process that writes one of them only by nonblocking assignments, at
/// least one of them at a run-time element, publishes just the elements it
/// wrote (see `Ff::lower_process`) instead of seeding and committing the
/// whole array on every edge.
///
/// This uses the drivers within `module`. The sparse commit copies back
/// only the 64-bit chunks the process wrote, so a write from another scope
/// that lands between the evaluation and the commit of the process can be
/// lost only within those chunks, where a whole-array commit loses it
/// everywhere. Arrays that processes of other modules write through
/// hierarchical names (`foreign_writes`) keep the whole-array commit, which
/// they are ordered against.
pub(super) fn edge_only_memories(
    module: &sv::ir::Module,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
    foreign_writes: &HashMap<String, hierarchy::ForeignWrites>,
) -> HashSet<String> {
    let first_ff = module.comb_processes().len();
    let end_ff = first_ff + module.ff_processes().len();
    // Only an array a nonblocking assignment of an edge process writes can
    // qualify; the driver scan is for those.
    let mut nonblocking = HashSet::default();
    for process in module.ff_processes() {
        for stmt in process.body() {
            stmt.walk(&mut |stmt| match stmt {
                sv::ir::Stmt::Assign {
                    lhs,
                    nonblocking: true,
                    ..
                } => {
                    nonblocking.insert(lhs.name());
                }
                sv::ir::Stmt::AssignConcat {
                    parts,
                    nonblocking: true,
                    ..
                } => nonblocking.extend(parts.iter().map(sv::ir::LValue::name)),
                _ => {}
            });
        }
    }
    let mut index = None;
    module
        .signals()
        .iter()
        .filter(|s| {
            !s.is_net()
                && !s.r#type().unpacked_ranges().is_empty()
                && nonblocking.contains(s.name())
                && !foreign_writes.contains_key(s.name())
        })
        .filter(|signal| {
            let drivers = index
                .get_or_insert_with(|| ModuleDrivers::new(module, constants, parameter_types))
                .local(signal.name());
            !drivers.is_empty()
                && drivers
                    .iter()
                    .all(|(id, _)| (first_ff..end_ff).contains(id))
        })
        .map(|signal| signal.name().to_string())
        .collect()
}
