//! Level-sensitive waits (IEEE 1800-2023 9.4.3) and the wait sites of a
//! process, which event controls suspend at too.

use super::*;

impl Ff<'_, '_> {
    /// `wait (condition)` (IEEE 1800-2023 9.4.3): the process goes on at once
    /// if the condition holds, and otherwise checks it again whenever a
    /// variable it reads changes.
    pub(super) fn wait_level(&mut self, condition: &sv::ir::Expr) -> Result<(), sv::AnalyzerError> {
        let ids = self.level_terms(condition)?;
        let site = self.wait_site(
            ids.into_iter()
                .map(|id| (id, CounterKind::Changes))
                .collect(),
            Vec::new(),
        );
        // A predicate that reads the status of a process or the trigger
        // state of an event observes them, which it tells the host, and its
        // site is a level wait's, which those wake.
        let observe = if self.kernel.is_some() {
            self.activation.process_control
        } else {
            reads_process_state(self.m, condition)
        };
        // Keep the kernel available while evaluating the predicate: functions
        // may perform synchronous process control such as disable fork.
        let direct = std::mem::replace(&mut self.direct, true);
        let predicate = std::mem::replace(&mut self.process_predicate, true);
        let result = (|| {
            if observe {
                self.process_call(celox_design::ProcessOp::Observe, &[])?;
            }
            let (holds, _) = self.eval_truth(condition)?;
            let join = self.b.new_block();
            let wait = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: holds,
                true_block: (join, Vec::new()),
                false_block: (wait, Vec::new()),
            });
            self.b.switch_to_block(wait);
            let point = self.suspend_at(site, observe)?;
            if observe {
                self.process_call(celox_design::ProcessOp::Observe, &[])?;
            }
            let (holds, _) = self.eval_truth(condition)?;
            self.wake_at(point, holds)?;
            self.b.seal_block(SIRTerminator::Jump(join, Vec::new()));
            self.b.switch_to_block(join);
            Ok(())
        })();
        self.direct = direct;
        self.process_predicate = predicate;
        result
    }

    /// Suspend the process at wait site `site` of the kernel or the unit
    /// being lowered, a level wait's if `level` (see
    /// [`celox_design::PROCESS_LEVEL_WAIT`]). The open block afterwards is
    /// the resume point, which [`Self::wake_at`] ends.
    pub(super) fn suspend_at(
        &mut self,
        site: u32,
        level: bool,
    ) -> Result<WaitPoint, sv::AnalyzerError> {
        if self.kernel.is_none() {
            let word = u64::from(site)
                | if level {
                    celox_design::PROCESS_LEVEL_WAIT
                } else {
                    0
                };
            let point = self.unit_wait(word)?;
            return Ok(WaitPoint::Unit { point, word });
        }
        self.with_kernel(|kernel| {
            if level {
                kernel.suspend_level_wait(site)
            } else {
                kernel.suspend_wait(site)
            }
        })
        .map_err(kernel_error)?;
        Ok(WaitPoint::Kernel)
    }

    /// At the resume point `point` of a wait: continue when `woken`, a
    /// one-bit register, holds, at a flush point of the process's deferred
    /// assertions (IEEE 1800-2023 16.4.2); otherwise wait at the same site
    /// again. The continuation is open afterwards.
    pub(super) fn wake_at(
        &mut self,
        point: WaitPoint,
        woken: RegisterId,
    ) -> Result<(), sv::AnalyzerError> {
        match point {
            WaitPoint::Kernel => {
                self.with_kernel(|kernel| kernel.wake_if(woken));
                Ok(())
            }
            WaitPoint::Unit { point, word } => {
                self.unit_wake_if(woken, point, word)?;
                self.unit_flush_point();
                Ok(())
            }
        }
    }

    /// The variables whose changes may change `condition`: the module
    /// variables it reads, also through the functions it calls, and the
    /// change counts of the class properties it reads (see
    /// `property_changes`), which a store of the property bumps.
    pub(super) fn level_terms(
        &mut self,
        condition: &sv::ir::Expr,
    ) -> Result<Vec<SourceVarId>, sv::AnalyzerError> {
        let names = level_reads(self.m, condition);
        // Static task formals can change when another caller copies arguments
        // in, but hidden local storage has no observable change counters yet.
        // Do not compile a wait that would silently miss that wakeup, including
        // dependencies read indirectly by a function in the condition.
        if names.iter().any(|name| {
            self.m
                .id(name)
                .is_some_and(|id| self.m.is_hidden(id) && self.is_static_task_formal(id))
        }) {
            return Err(unsupported("wait condition reading a static task argument"));
        }
        // A store of a class property bumps its change count, which the
        // wait watches; a property without one, such as in a design whose
        // waits were not collected, or a random mode, is never checked again.
        let mut changes = Vec::new();
        let reads = heap_reads(|name| self.m.subroutine(name), condition);
        // A store of the same property of another object, or of its own
        // value, or a `randomize` checks the condition again: it must be a
        // value of what it reads, so that an extra evaluation is invisible.
        if !reads.is_empty()
            && let Some(name) = impure_call_reading(self.m, condition, |op| {
                matches!(
                    op,
                    sv::ir::ObjectOp::Load(_)
                        | sv::ir::ObjectOp::ClassOf
                        | sv::ir::ObjectOp::Box(_)
                        | sv::ir::ObjectOp::Locator(_)
                        | sv::ir::ObjectOp::Concat(_)
                ) || matches!(op, sv::ir::ObjectOp::Container { op, .. } if !op.mutates())
            })
        {
            return Err(unsupported(format!(
                "{name} in a wait condition that reads a class property"
            )));
        }
        for op in reads {
            match &op {
                sv::ir::ObjectOp::Load(field) if let Some(id) = self.m.change_counter(field) => {
                    changes.push(id);
                }
                _ => return Err(unsupported(format!("{op} in a wait condition"))),
            }
        }
        let mut ids: Vec<_> = names
            .iter()
            .filter_map(|name| self.m.id(name))
            .filter(|&id| {
                !self.m.is_hidden(id)
                    || (self.activation.forked && self.activation.frame_locals.contains(&id))
            })
            .chain(changes)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// Declare a wait site of the process with the given terms and return
    /// its local index.
    pub(super) fn wait_site(
        &mut self,
        terms: Vec<(SourceVarId, CounterKind)>,
        gates: Vec<Option<WaitGate<SourceVarId>>>,
    ) -> u32 {
        // A unit's site is one of the module's function sites, which the
        // kernels that call it list after their own.
        if self.kernel.is_none() {
            let index = self.m.function_wait_sites.len() as u32;
            self.m.function_wait_sites.push(WaitSite {
                process: 0,
                terms,
                gates,
            });
            return index;
        }
        let index = self.sites.wait.len() as u32;
        self.sites.wait.push(WaitSite {
            process: self.sites.process,
            terms,
            gates,
        });
        index
    }
}

/// The names a process waiting on the value of `expr` watches: those `expr`
/// reads, and those the subroutines it calls read, directly or through
/// further calls, the defaults of their formals included. A level wait
/// (IEEE 1800-2023 9.4.3) and an activation-local event expression are
/// woken by the changes of these, and the out-of-line eligibility of
/// subroutines counts their stores as observed through this same walk
/// (see `ProcModule::observe_storage`).
pub(in crate::lowering) fn level_reads(
    pm: &ProcModule<'_>,
    expr: &sv::ir::Expr,
) -> HashSet<String> {
    let mut names = HashSet::default();
    expr_idents(expr, &mut names);
    subroutine_reads(pm, expr, &mut names);
    names
}

/// The module variables the subroutines `expr` calls read, directly or
/// through further calls, besides their own formals and locals.
pub(super) fn subroutine_reads(
    pm: &ProcModule<'_>,
    expr: &sv::ir::Expr,
    names: &mut HashSet<String>,
) {
    let mut pending = Vec::new();
    collect_call_names(expr, &mut pending);
    let mut visited = HashSet::default();
    while let Some(name) = pending.pop() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let Some(subroutine) = pm.subroutine(&name) else {
            continue;
        };
        for param in &subroutine.params {
            if let Some(default) = &param.default {
                expr_idents(default, names);
                collect_call_names(default, &mut pending);
            }
        }
        for stmt in &subroutine.body {
            let mut stmt = stmt.clone();
            stmt.visit_mut(
                &mut |expr| {
                    expr_idents(expr, names);
                    collect_call_names(expr, &mut pending);
                },
                &mut |_| {},
                &mut |_| {},
            );
            stmt.walk(&mut |stmt| {
                if let sv::ir::Stmt::Call { name, .. } = stmt {
                    pending.push(name.clone());
                }
            });
        }
    }
}

/// The operations in `expr` that read the class object heap, directly or
/// through the functions it calls, which `subroutine` finds by name: class
/// properties, and the random modes of them. Container contents are values
/// of their handles. The default of an argument counts only where a call
/// omits the argument.
pub(in crate::lowering) fn heap_reads<'a>(
    subroutine: impl Fn(&str) -> Option<&'a sv::ir::Subroutine>,
    expr: &sv::ir::Expr,
) -> Vec<sv::ir::ObjectOp> {
    let mut reads = Vec::new();
    let mut pending = vec![expr.clone()];
    let mut calls = Vec::new();
    let mut visited = HashSet::default();
    let mut defaults = HashSet::default();
    loop {
        while let Some(expr) = pending.pop() {
            expr_calls(&expr, &mut |callee| {
                if let sv::ir::Callee::Object(op) = callee
                    && matches!(
                        **op,
                        sv::ir::ObjectOp::Load(_)
                            | sv::ir::ObjectOp::ModeGet(..)
                            | sv::ir::ObjectOp::ConstraintModeGet(_)
                    )
                    && !reads.contains(&**op)
                {
                    reads.push((**op).clone());
                }
                false
            });
            collect_calls(&expr, &mut calls);
        }
        let Some((callee, args)) = calls.pop() else {
            return reads;
        };
        let sv::ir::Callee::Subroutine(name) = callee else {
            continue;
        };
        let Some(subroutine) = subroutine(&name) else {
            continue;
        };
        for (position, param) in subroutine.params.iter().enumerate() {
            if matches!(args.get(position), None | Some(None))
                && let Some(default) = &param.default
                && defaults.insert((name.clone(), position))
            {
                pending.push(default.clone());
            }
        }
        if !visited.insert(name) {
            continue;
        }
        for stmt in &subroutine.body {
            let mut stmt = stmt.clone();
            stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
            stmt.visit_mut(
                &mut |expr| pending.push(expr.clone()),
                &mut |_| {},
                &mut |_| {},
            );
        }
    }
}

/// The keys of the class properties that the level waits of `module` read
/// (IEEE 1800-2023 9.4.3), in its processes and subroutines: a store of
/// one of them must wake the waits (see [`property_changes`]).
pub(crate) fn watched_properties(
    module: &sv::ir::Module,
    keys: &mut std::collections::BTreeSet<u32>,
) {
    let subroutine = |name: &str| {
        module
            .subroutines()
            .iter()
            .find(|subroutine| subroutine.name == name)
    };
    let mut conditions = Vec::new();
    let mut events = Vec::new();
    let mut visit = |stmts: &[sv::ir::Stmt]| {
        for stmt in stmts {
            stmt.walk(&mut |stmt| match stmt {
                sv::ir::Stmt::Wait(condition) => conditions.push(condition.clone()),
                sv::ir::Stmt::WaitEvent(items) => {
                    events.extend(items.iter().map(|item| item.expr.clone()));
                }
                _ => {}
            });
        }
    };
    for process in module.initial_processes() {
        visit(process.body());
    }
    for process in module.ff_processes() {
        visit(process.body());
    }
    for process in module.comb_processes() {
        visit(process.body());
    }
    for subroutine in module.subroutines() {
        visit(&subroutine.body);
    }
    for condition in &conditions {
        for op in heap_reads(subroutine, condition) {
            if let sv::ir::ObjectOp::Load(field) = op
                && let sv::ir::ChangeKey::Property(key) = field.key
            {
                keys.insert(key);
            }
        }
    }
    // An event control on a property waits for the stores of its key (see
    // `Ff::property_event`); the heap counts the occurrences in each object.
    for expr in &events {
        for op in heap_reads(subroutine, expr) {
            if let sv::ir::ObjectOp::Load(field) = op
                && let sv::ir::ChangeKey::Property(key) = field.key
            {
                keys.insert(key);
            }
        }
    }
}

/// The name of the variable that counts the stores of the class property
/// with key `key`, which the level waits that read the property watch: a
/// variable of the package [`PROPERTY_CHANGES`], shared by every module.
pub(crate) fn property_changes(key: u32) -> String {
    format!("$property_changes@{key:08x}")
}

/// The package that holds the [`property_changes`] variables.
pub(crate) const PROPERTY_CHANGES: &str = "$property_changes";

/// Bits of a [`property_changes`] variable.
pub(crate) const PROPERTY_CHANGES_WIDTH: usize = 32;

/// Where a wait of a kernel or a unit resumes (see `Ff::suspend_at`).
#[derive(Clone, Copy)]
pub(super) enum WaitPoint {
    Kernel,
    /// The unit's resume point and the delay word of its wait.
    Unit {
        point: u32,
        word: u64,
    },
}

/// Whether `condition` reads, also through the subroutines it calls, the
/// state of a process or of an event that only the host knows, such as a
/// process's `status()` or an event's `triggered` (IEEE 1800-2023 9.7,
/// 15.5.3).
pub(super) fn reads_process_state(pm: &ProcModule<'_>, condition: &sv::ir::Expr) -> bool {
    let mut calls = Vec::new();
    collect_calls(condition, &mut calls);
    let mut visited = HashSet::default();
    while let Some((callee, args)) = calls.pop() {
        match callee {
            sv::ir::Callee::System(sv::ir::SystemFunction::Process(_)) => return true,
            sv::ir::Callee::Subroutine(name) => {
                let Some(subroutine) = pm.subroutine(&name) else {
                    continue;
                };
                default_calls(subroutine, &args, &mut calls, &mut Default::default());
                if visited.insert(name) {
                    for stmt in &subroutine.body {
                        stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
                    }
                }
            }
            _ => {}
        }
    }
    false
}
