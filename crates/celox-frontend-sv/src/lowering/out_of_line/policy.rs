//! Which calls go out of line: the storage of the module's subroutines,
//! the subroutines on a cycle of the call graph, and why a subroutine cannot
//! be lowered into a function unit.

use super::*;

impl ProcModule<'_> {
    /// The out-of-line functions, when calls go out of line.
    pub(in crate::lowering) fn out_of_line(&mut self) -> Option<&mut OutOfLine> {
        self.calls.as_deref_mut()?.out_of_line.as_mut()
    }

    /// The automatic storage of the module's subroutines (see
    /// [`automatic_names`]). Such a variable is a temporary of each inline
    /// expansion, which a function unit keeps in its frame.
    /// The declared variable that `id` stands for: `id` itself, or the
    /// variable an inlined activation copied it from, through every
    /// activation of a recursion. A function's frame has slots for the
    /// declared variables, whatever activation a call is made in.
    pub(in crate::lowering) fn declared(&self, mut id: SourceVarId) -> SourceVarId {
        while let Some(&(_, original, _)) = self
            .activation_copies
            .iter()
            .rev()
            .find(|&&(_, _, copy)| copy == id)
        {
            id = original;
        }
        id
    }

    /// Point the names that inlined activations copied at the declared
    /// variables again, and return the names to give back to
    /// [`Self::restore_names`]. A function unit is lowered once and serves
    /// every call, so it must not capture the copies of the activation its
    /// first call happens to be lowered in.
    pub(in crate::lowering) fn declare_names(&mut self) -> Vec<(String, SourceVarId)> {
        let current: Vec<(String, SourceVarId)> = self
            .activation_copies
            .iter()
            .map(|(name, _, _)| (name.clone(), self.name_to_id[name]))
            .collect();
        for (name, original, _) in self.activation_copies.iter().rev() {
            self.name_to_id.insert(name.clone(), *original);
        }
        current
    }

    pub(in crate::lowering) fn restore_names(&mut self, names: Vec<(String, SourceVarId)>) {
        for (name, id) in names {
            self.name_to_id.insert(name, id);
        }
    }

    pub(in crate::lowering) fn automatic_storage(&self) -> HashSet<SourceVarId> {
        self.subroutines
            .values()
            .flat_map(automatic_names)
            .filter_map(|name| self.id(name))
            .collect()
    }

    /// The static storage of the module's subroutines: what they declare,
    /// formals and results included, that is not automatic. It is shared by
    /// every call, inlined or not, recursive ones included.
    pub(in crate::lowering) fn static_storage(&self) -> HashSet<SourceVarId> {
        self.subroutines
            .values()
            .flat_map(static_names)
            .filter_map(|name| self.id(name))
            .collect()
    }

    /// Whether assembly may instrument the stores into `id` (see
    /// [`Self::observe_storage`]).
    pub(in crate::lowering) fn is_observed(&self, id: SourceVarId) -> bool {
        self.calls
            .as_deref()
            .and_then(|graph| graph.out_of_line.as_ref())
            .is_some_and(|functions| functions.observed.contains(&id))
    }

    /// Whether assembly instruments the stores into `id` in ways only the
    /// units of processes and flip-flops carry (see
    /// [`Self::observe_storage`]), so that a function unit must not make
    /// them.
    pub(in crate::lowering) fn is_excluded(&self, id: SourceVarId) -> bool {
        self.calls
            .as_deref()
            .and_then(|graph| graph.out_of_line.as_ref())
            .is_some_and(|functions| functions.excluded.contains(&id))
    }

    /// Record, before `module`'s processes are lowered, the variables whose
    /// stores assembly may instrument (`SimModule::instrumented_storage`),
    /// as far as a function unit can store them.
    ///
    /// A function unit stores no hidden module state but the static storage
    /// of subroutines. Assembly instruments the stores into what a wait or an
    /// event control watches, what combinational logic is sensitive to, and
    /// the targets of nonblocking assignments. Each construct is walked by
    /// the walk its lowering derives that instrumentation from, so the two
    /// cannot disagree: a level wait by [`super::super::ff::level_reads`],
    /// combinational logic, `@*` and the expressions of event controls
    /// (whose values are continuous assignments of the module) by
    /// [`Self::implicit_reads`], an event control on an activation-local
    /// expression by `level_reads` too. Added are the targets of
    /// nonblocking assignments and `foreign`, the names that hierarchical
    /// names of other modules reach. The set may be more than assembly
    /// instruments, which only keeps a subroutine inlined that could have
    /// gone out of line.
    ///
    /// Assembly instruments a function unit's stores as a process kernel's
    /// for the counters of waits and event controls, which are kept for the
    /// stored variable, whatever process stores it, the occurrences of
    /// flip-flop events included. Its other instrumentation is for process
    /// and flip-flop units only: the triggers that combinational and
    /// flip-flop units' stores into events get, the staging of nonblocking
    /// assignments, the counting of what flip-flops write, and the
    /// captures of combinational observers in combinational processes. So a
    /// function unit stores into none of the `excluded` storage: the writes
    /// of flip-flops, the targets of nonblocking assignments and `foreign`;
    /// and a combinational process calls no function unit that stores into
    /// `observed` storage, module state included. Assembly rejects a
    /// function unit that a combinational or flip-flop unit calls and that
    /// stores into an event, and one that stores into storage it cannot
    /// instrument.
    pub(in crate::lowering) fn observe_storage(
        &mut self,
        module: &sv::ir::Module,
        foreign: &HashSet<String>,
    ) {
        if self.out_of_line().is_none() {
            return;
        }
        let assign = |lhs: &sv::ir::LValue, rhs: &sv::ir::Expr| sv::ir::Stmt::Assign {
            lhs: lhs.clone(),
            rhs: rhs.clone(),
            nonblocking: false,
        };
        let mut observers: Vec<sv::ir::Stmt> = Vec::new();
        // What a function unit must not store into: what flip-flops write,
        // and below the targets of nonblocking assignments and the names of
        // other modules. Events are assembly's to judge, which sees the
        // units that call each function.
        let mut hard_names: Vec<String> = Vec::new();
        for process in module.ff_processes() {
            let mut written = HashSet::default();
            super::super::procedural::written_names(process.body(), &mut written);
            hard_names.extend(written);
        }
        for assignment in module.assignments() {
            observers.push(assign(assignment.lhs_value(), assignment.rhs()));
        }
        for process in module.comb_processes() {
            for assignment in process.assignments() {
                observers.push(assign(assignment.lhs_value(), assignment.rhs()));
            }
            observers.extend(process.body().iter().cloned());
            observers.extend(process.sensitivity_body().iter().cloned());
        }
        for instance in module.instances() {
            for connection in instance.port_connections() {
                if let Some(actual) = connection.actual_expr() {
                    observers.push(sv::ir::Stmt::Eval(actual.clone()));
                }
            }
        }
        let mut levels: Vec<&sv::ir::Expr> = Vec::new();
        let mut targets: Vec<&str> = Vec::new();
        let bodies =
            module
                .initial_processes()
                .iter()
                .map(|process| process.body())
                .chain(module.ff_processes().iter().map(|process| process.body()))
                .chain(module.comb_processes().iter().map(|process| process.body()))
                .chain(module.subroutines().iter().flat_map(|subroutine| {
                    [&subroutine.body[..], &subroutine.sensitivity_body[..]]
                }));
        for body in bodies {
            for stmt in body {
                stmt.walk(&mut |stmt| match stmt {
                    sv::ir::Stmt::Wait(condition) => levels.push(condition),
                    sv::ir::Stmt::WaitEvent(items) => {
                        for item in items {
                            for expr in std::iter::once(&item.expr).chain(&item.iff) {
                                observers.push(sv::ir::Stmt::Eval(expr.clone()));
                                levels.push(expr);
                            }
                        }
                    }
                    // `walk` does not visit the statements whose reads an
                    // `@*` covers.
                    sv::ir::Stmt::WaitImplicit { reads, .. } => {
                        observers.extend(reads.iter().cloned());
                    }
                    sv::ir::Stmt::Assign {
                        lhs,
                        nonblocking: true,
                        ..
                    } => targets.push(lhs.name()),
                    sv::ir::Stmt::AssignConcat {
                        parts,
                        nonblocking: true,
                        ..
                    } => targets.extend(parts.iter().map(sv::ir::LValue::name)),
                    _ => {}
                });
            }
        }
        let mut observed: HashSet<SourceVarId> = self
            .implicit_reads(&observers, true)
            .into_iter()
            .map(|atom| atom.id)
            .collect();
        let levels: Vec<String> = levels
            .into_iter()
            .flat_map(|expr| super::super::ff::level_reads(self, expr))
            .collect();
        let mut excluded: HashSet<SourceVarId> = HashSet::default();
        excluded.extend(
            targets
                .iter()
                .copied()
                .chain(foreign.iter().map(String::as_str))
                .chain(hard_names.iter().map(String::as_str))
                .filter_map(|name| self.id(name)),
        );
        observed.extend(
            targets
                .into_iter()
                .chain(levels.iter().map(String::as_str))
                .chain(foreign.iter().map(String::as_str))
                .filter_map(|name| self.id(name)),
        );
        observed.extend(excluded.iter().copied());
        if let Some(functions) = self.out_of_line() {
            functions.observed = observed;
            functions.excluded = excluded;
        }
    }

    /// Record, before the module's processes are lowered, the subroutines
    /// on a cycle of the module's call graph: those whose bodies, the
    /// argument defaults of their calls included, may call themselves
    /// again (see [`ProcModule::single_activation`]).
    pub(in crate::lowering) fn mark_recursive(&mut self, module: &sv::ir::Module) {
        if self.out_of_line().is_none() {
            return;
        }
        let mut names: Vec<&String> = self.subroutines.keys().collect();
        names.sort_unstable();
        let index: HashMap<&str, usize> = names
            .iter()
            .enumerate()
            .map(|(position, name)| (name.as_str(), position))
            .collect();
        // The call graph over calls: a node for each subroutine with each
        // pattern of omitted arguments it is called with, one for each
        // distinct pair however many call paths reach it. The node with
        // nothing omitted is the body; a node omitting arguments is a call
        // that runs the body and the defaults of those arguments (IEEE
        // 1800-2023 13.5.3). Each node's sites are the nodes it calls, once
        // per call, which size it; recursion is a cycle of this graph.
        let mut nodes: HashMap<(usize, Vec<bool>), usize> = HashMap::default();
        let mut keys: Vec<(usize, Vec<bool>)> = Vec::new();
        let mut node = |key: (usize, Vec<bool>), keys: &mut Vec<_>| -> usize {
            *nodes.entry(key.clone()).or_insert_with(|| {
                keys.push(key);
                keys.len() - 1
            })
        };
        // The node a call makes: the omitted arguments that take a default.
        let site = |(callee, args): (sv::ir::Callee, Vec<Option<sv::ir::Expr>>)| {
            let sv::ir::Callee::Subroutine(callee) = callee else {
                return None;
            };
            let &target = index.get(callee.as_str())?;
            let mut omitted: Vec<bool> = self.subroutines[names[target]]
                .params
                .iter()
                .enumerate()
                .map(|(position, param)| {
                    param.default.is_some() && matches!(args.get(position), None | Some(None))
                })
                .collect();
            if !omitted.contains(&true) {
                omitted.clear();
            }
            Some((target, omitted))
        };
        for target in 0..names.len() {
            node((target, Vec::new()), &mut keys);
        }
        // The calls of the module's processes, continuous assignments and
        // port connections, which reach the nodes of their omitted arguments.
        let mut roots = Vec::new();
        let bodies = module
            .initial_processes()
            .iter()
            .map(|process| process.body())
            .chain(module.ff_processes().iter().map(|process| process.body()))
            .chain(
                module
                    .comb_processes()
                    .iter()
                    .flat_map(|process| [process.body(), process.sensitivity_body()]),
            );
        for body in bodies {
            for stmt in body {
                stmt.walk(&mut |stmt| stmt_calls(stmt, &mut roots));
            }
        }
        for assignment in module.assignments().iter().chain(
            module
                .comb_processes()
                .iter()
                .flat_map(|process| process.assignments()),
        ) {
            collect_calls(assignment.rhs(), &mut roots);
        }
        for instance in module.instances() {
            for connection in instance.port_connections() {
                if let Some(actual) = connection.actual_expr() {
                    collect_calls(actual, &mut roots);
                }
            }
        }
        for key in roots.into_iter().filter_map(site) {
            node(key, &mut keys);
        }
        // Each node's own statements and sites, in the order nodes appear;
        // a node's sites may add nodes, sized in turn.
        let mut own: Vec<u64> = Vec::new();
        let mut sites: Vec<Vec<usize>> = Vec::new();
        while own.len() < keys.len() {
            let (target, omitted) = keys[own.len()].clone();
            let subroutine = &self.subroutines[names[target]];
            let mut calls = Vec::new();
            let mut statements = 0u64;
            let mut own_sites = Vec::new();
            if omitted.is_empty() {
                for stmt in subroutine.body.iter().chain(&subroutine.sensitivity_body) {
                    stmt.walk(&mut |stmt| {
                        statements += 1;
                        stmt_calls(stmt, &mut calls)
                    });
                }
            } else {
                own_sites.push(node((target, Vec::new()), &mut keys));
                for (param, _) in subroutine
                    .params
                    .iter()
                    .zip(&omitted)
                    .filter(|(_, omitted)| **omitted)
                {
                    statements += 1;
                    collect_calls(
                        param.default.as_ref().expect("a defaulted formal"),
                        &mut calls,
                    );
                }
            }
            for key in calls.into_iter().filter_map(site) {
                own_sites.push(node(key, &mut keys));
            }
            own.push(statements);
            sites.push(own_sites);
        }
        let calls: Vec<Vec<usize>> = sites
            .iter()
            .map(|callees| {
                let mut callees = callees.clone();
                callees.sort_unstable();
                callees.dedup();
                callees
            })
            .collect();
        let recursive_nodes: HashSet<usize> = super::super::recursion::recursive_components(&calls)
            .into_iter()
            .flatten()
            .collect();
        let recursive_targets: HashSet<usize> =
            recursive_nodes.iter().map(|&node| keys[node].0).collect();
        // A call of a recursive subroutine inlines nothing, whatever it omits.
        let members: HashSet<usize> = (0..keys.len())
            .filter(|&node| recursive_targets.contains(&keys[node].0))
            .collect();
        let sizes = fully_inlined_sizes(&own, &sites, &members);
        // A subroutine's size is that of its largest call: one policy holds
        // for all its calls, and a call that inlines its defaults inlines
        // their calls too.
        let mut inlined_sizes: HashMap<String, u64> = HashMap::default();
        for (node, size) in sizes.into_iter().enumerate() {
            let entry = inlined_sizes
                .entry(names[keys[node].0].clone())
                .or_default();
            *entry = (*entry).max(size);
        }
        let recursive: HashSet<String> = recursive_targets
            .iter()
            .map(|&target| names[target].clone())
            .collect();
        if let Some(functions) = self.out_of_line() {
            functions.recursive = recursive;
            functions.inlined_sizes = inlined_sizes;
        }
    }

    /// Whether a call of subroutine `name` goes out of line whatever its
    /// arguments (see [`Self::out_of_line_policy`]), even constant ones,
    /// which would otherwise keep it inlined to fold: when `name` is on a
    /// cycle of the call graph, whose calls inlining cannot expand, or over
    /// the size threshold, whose body does not fold smaller.
    pub(in crate::lowering) fn out_of_line_whatever_arguments(&self, name: &str) -> bool {
        self.calls
            .as_deref()
            .and_then(|graph| graph.out_of_line.as_ref())
            .is_some_and(|functions| {
                functions.recursive.contains(name)
                    || (!functions.forced
                        && functions
                            .inlined_size(name)
                            .is_some_and(|size| size > OUT_OF_LINE_STATEMENTS))
            })
    }

    /// Whether a call of subroutine `name` may go out of line: always when
    /// that is forced; otherwise when `name` is on a cycle of the call
    /// graph, which inlining cannot expand, or when its fully inlined size
    /// exceeds [`OUT_OF_LINE_STATEMENTS`]. Every other call is inlined, as
    /// without out-of-line calls.
    ///
    /// The size is measured on the IR before anything is lowered (see
    /// [`Self::mark_recursive`]), so the choice does not depend on the order
    /// of lowering.
    pub(in crate::lowering) fn out_of_line_policy(&self, name: &str) -> bool {
        self.calls
            .as_deref()
            .and_then(|graph| graph.out_of_line.as_ref())
            .is_some_and(|functions| {
                functions.forced
                    || functions.recursive.contains(name)
                    || functions
                        .inlined_size(name)
                        .is_some_and(|size| size > OUT_OF_LINE_STATEMENTS)
            })
    }

    /// Whether at most one activation of the unit of subroutine `name` can
    /// be live at a time, so that its automatic unpacked arrays, which a
    /// frame does not hold, may stay module variables (each activation
    /// initializes them as its declarations say, as an inlined body does).
    ///
    /// A second activation could start only while one is live: by a call
    /// from inside it, which only a cycle of the call graph makes (the
    /// subroutine is not on one); or by another process while the first is
    /// suspended, which only a suspension past the moment lets run (the body
    /// has no delay and no `$stop`, nor do the bodies it reaches; a host
    /// request is served before anything else runs, and `$finish` ends the
    /// process). Processes run one at a time, so no other runs in between.
    pub(in crate::lowering) fn single_activation(&mut self, name: &str) -> bool {
        let recursive = self
            .out_of_line()
            .is_none_or(|functions| functions.recursive.contains(name));
        !recursive && !self.process_state(name).yields
    }

    /// Why subroutine `name` cannot go out of line wherever it is called,
    /// if it cannot: a property of its body and of the bodies it reaches.
    pub(in crate::lowering) fn out_of_line_reason(&mut self, name: &str) -> Option<Reason> {
        if let Some(known) = self
            .out_of_line()
            .and_then(|functions| functions.intrinsic.get(name))
        {
            return known.clone();
        }
        let (reason, state) = self.find_out_of_line_reason(name);
        if let Some(functions) = self.out_of_line() {
            functions.intrinsic.insert(name.to_string(), reason.clone());
            functions.process_state.insert(name.to_string(), state);
        }
        reason
    }

    /// The process state that subroutine `name` and the bodies it reaches
    /// use (see [`ProcessState`]).
    pub(in crate::lowering) fn process_state(&mut self, name: &str) -> ProcessState {
        self.out_of_line_reason(name);
        self.out_of_line()
            .and_then(|functions| functions.process_state.get(name).copied())
            .unwrap_or_default()
    }

    pub(super) fn find_out_of_line_reason(&mut self, name: &str) -> (Option<Reason>, ProcessState) {
        let mut state = ProcessState::default();
        let mut found: Option<Reason> = None;
        let mut note = |reason: Reason| {
            if found.as_ref().is_none_or(|known| reason < *known) {
                found = Some(reason);
            }
        };
        // The formals are the function's interface: a callee whose formals
        // a unit cannot pass stays inlined, and its body is then part of
        // the caller's unit, which binds them as any inlined call does.
        if let Some(subroutine) = self.subroutine(name) {
            for param in &subroutine.params {
                // A `ref` formal of unpacked structure type binds each member
                // to a member of the actual, which a place does not express.
                if param.direction.is_reference() && !param.leaves.is_empty() {
                    note(Reason::RefFormal);
                }
                if !param.leaves.is_empty() {
                    note(Reason::Other("unpacked structure formal".to_string()));
                }
            }
        }
        // What the body does, and the bodies of the subroutines it calls,
        // which are inlined into the unit unless they go out of line
        // themselves, and can only if they qualify themselves.
        let mut pending = vec![name.to_string()];
        let mut visited = HashSet::default();
        while let Some(name) = pending.pop() {
            if !visited.insert(name.clone()) {
                continue;
            }
            let Some(subroutine) = self.subroutine(&name) else {
                note(Reason::Other(format!("no subroutine `{name}`")));
                continue;
            };
            let mut calls = Vec::new();
            for stmt in &subroutine.body {
                stmt.walk(&mut |stmt| {
                    match stmt {
                        // A delay suspends the process with the unit's
                        // frames, which the runtime keeps with the
                        // activation until the delay ends.
                        sv::ir::Stmt::Delay { .. } => {
                            state.suspends = true;
                            state.yields = true;
                        }
                        // A fork creates processes that run the unit's
                        // code in copies of its frames, and the parent
                        // goes on; `wait fork` parks the process until
                        // they end.
                        sv::ir::Stmt::Fork { .. } | sv::ir::Stmt::WaitFork => {
                            state.suspends = true;
                            state.yields = true;
                            state.forks = true;
                        }
                        sv::ir::Stmt::DisableFork => {
                            state.suspends = true;
                            state.forks = true;
                        }
                        // A wait suspends the process with the unit's
                        // frames until the occurrence it waits for.
                        sv::ir::Stmt::WaitEvent(_) | sv::ir::Stmt::Wait(_) => {
                            state.suspends = true;
                            state.yields = true;
                            state.waits = true;
                        }
                        sv::ir::Stmt::WaitImplicit { .. }
                        | sv::ir::Stmt::DelayedNonblocking { .. }
                        | sv::ir::Stmt::DeferActive
                        | sv::ir::Stmt::AssertionRegion { .. } => note(Reason::MaySuspend),
                        // A nonblocking assignment hands the runtime its
                        // update by suspending; the process goes on at once.
                        sv::ir::Stmt::Assign {
                            nonblocking: true, ..
                        }
                        | sv::ir::Stmt::AssignConcat {
                            nonblocking: true, ..
                        } => {
                            state.suspends = true;
                            state.nbas = true;
                        }
                        // Prints, messages and Veryl's assertion calls are
                        // runtime events, which a unit raises as an inlined
                        // body does; a message carries the time. `$finish`
                        // and `$stop` end or suspend the process, a deferred
                        // action waits in the process's queue (IEEE 1800-2023
                        // 16.4.1), and the other tasks are host requests;
                        // `$exit` depends on the program it runs in.
                        sv::ir::Stmt::SystemTask {
                            task, assertion, ..
                        } => match system_task_kind(*task) {
                            _ if assertion.as_ref().is_some_and(|assertion| {
                                assertion.deferral != celox_design::AssertionDeferral::None
                            }) =>
                            {
                                note(Reason::HostRequest);
                            }
                            Some(SystemTaskKind::Print(..) | SystemTaskKind::Assertion(_)) => {}
                            Some(SystemTaskKind::Message(_) | SystemTaskKind::Fatal) => {
                                state.time = true;
                            }
                            // File output suspends for its host request.
                            None if matches!(
                                task,
                                sv::ir::SystemTask::Fdisplay(_)
                                    | sv::ir::SystemTask::Fwrite(_)
                                    | sv::ir::SystemTask::Fclose
                                    | sv::ir::SystemTask::Fflush
                            ) =>
                            {
                                state.suspends = true;
                            }
                            // `$finish` ends and `$stop` suspends the
                            // process, the unit's frames with it.
                            Some(SystemTaskKind::Control(RuntimeEventKind::Finish)) => {
                                state.suspends = true;
                            }
                            Some(SystemTaskKind::Control(RuntimeEventKind::Stop)) => {
                                state.suspends = true;
                                state.yields = true;
                            }
                            Some(SystemTaskKind::Control(_)) | None => note(Reason::HostRequest),
                        },
                        _ => {}
                    }
                    stmt_calls(stmt, &mut calls);
                });
            }
            let mut seen_defaults = super::super::procedural::SeenDefaults::default();
            while let Some((callee, args)) = calls.pop() {
                match callee {
                    sv::ir::Callee::System(function) => {
                        use sv::ir::SystemFunction;
                        match function {
                            // `process::self()`, the RNG state of a process,
                            // a new semaphore and the triggers of events are
                            // host requests a unit makes by suspending: none
                            // parks the process, which continues as soon as
                            // the host served it. The calling process holds
                            // its RNG state while the call is suspended: the
                            // frames that hold it give it back first. The
                            // other process methods act on the calling
                            // process, and some park it.
                            SystemFunction::Process(
                                celox_design::ProcessOp::SelfHandle
                                | celox_design::ProcessOp::Srandom
                                | celox_design::ProcessOp::GetRandstate
                                | celox_design::ProcessOp::SetRandstate
                                | celox_design::ProcessOp::SemaphoreNew
                                | celox_design::ProcessOp::Trigger
                                | celox_design::ProcessOp::TriggerNonblocking
                                | celox_design::ProcessOp::Kill
                                | celox_design::ProcessOp::Resume
                                | celox_design::ProcessOp::SemaphorePut
                                | celox_design::ProcessOp::SemaphoreTryGet,
                            ) => state.suspends = true,
                            // `suspend()` of the calling process parks it
                            // until another process resumes it, `await()`
                            // until another ends, an event control on named
                            // events or a wait for a trigger until one is
                            // triggered, and the blocking methods of
                            // semaphores and mailboxes until they can be
                            // served; the frames stay with the activation
                            // meanwhile, and a mailbox method retries when the
                            // process is woken.
                            SystemFunction::Process(
                                celox_design::ProcessOp::Suspend
                                | celox_design::ProcessOp::Await
                                | celox_design::ProcessOp::SemaphoreGet
                                | celox_design::ProcessOp::WaitEvent
                                | celox_design::ProcessOp::WaitTriggered
                                | celox_design::ProcessOp::MailboxPut
                                | celox_design::ProcessOp::MailboxGet
                                | celox_design::ProcessOp::MailboxPeek,
                            ) => {
                                state.suspends = true;
                                state.yields = true;
                            }
                            SystemFunction::Process(
                                celox_design::ProcessOp::MailboxNew
                                | celox_design::ProcessOp::MailboxNum
                                | celox_design::ProcessOp::MailboxTryPut
                                | celox_design::ProcessOp::MailboxTryGet
                                | celox_design::ProcessOp::MailboxTryPeek,
                            ) => state.suspends = true,
                            // A level wait whose predicate reads them is
                            // waiting on the event or process too, which
                            // only the predicate's own code can tell the
                            // host; a unit's code serves every caller.
                            SystemFunction::Process(
                                celox_design::ProcessOp::Triggered
                                | celox_design::ProcessOp::Status,
                            ) => {
                                state.suspends = true;
                                state.observes_events = true;
                            }
                            SystemFunction::Process(_) => note(Reason::HostRequest),
                            _ if function.is_host_request() => state.suspends = true,
                            SystemFunction::Urandom
                            | SystemFunction::UrandomRange
                            | SystemFunction::ScopeRandomize
                            | SystemFunction::ScopeRandomizeEnum => state.random = true,
                            SystemFunction::Random if args.is_empty() => state.dist_seed = true,
                            // They advance the seed in their argument.
                            SystemFunction::Random | SystemFunction::DistUniform => {}
                            SystemFunction::Time
                            | SystemFunction::Stime
                            | SystemFunction::Realtime => state.time = true,
                            // The sampled values are those of the calling
                            // process's site (IEEE 1800-2023 16.9.3).
                            _ if function.reads_simulation_state() => {
                                note(Reason::Other("reads simulation state".to_string()));
                            }
                            _ => {}
                        }
                    }
                    sv::ir::Callee::String(sv::ir::StringOp::Format { .. }) => {
                        state.suspends = true;
                    }
                    sv::ir::Callee::Object(op) => match &*op {
                        // Construction seeds the object from the process
                        // RNG (IEEE 1800-2023 18.14.3), and `shuffle` draws
                        // on it.
                        sv::ir::ObjectOp::New { .. }
                        | sv::ir::ObjectOp::Container {
                            op: sv::ir::ContainerOp::Shuffle,
                            ..
                        } => state.random = true,
                        // The rest are heap calls, which need nothing but
                        // the heap: randomization and the object RNG draw
                        // on the object's own RNG, which is in it.
                        _ => {}
                    },
                    sv::ir::Callee::String(_)
                    | sv::ir::Callee::Checked { .. }
                    | sv::ir::Callee::Lookup { .. }
                    | sv::ir::Callee::Stream(_) => {}
                    sv::ir::Callee::Subroutine(callee) => {
                        if self.dpi_imports.contains_key(&callee) {
                            note(Reason::HostRequest);
                        } else if let Some(subroutine) = self.subroutine(&callee) {
                            default_calls(subroutine, &args, &mut calls, &mut seen_defaults);
                            pending.push(callee);
                        } else {
                            note(Reason::Other(format!("no subroutine `{callee}`")));
                        }
                    }
                }
            }
        }
        (found, state)
    }

    /// Whether a call of `subroutine` with `args` has only constant
    /// arguments, its defaults included. Such a call stays inlined: the
    /// inlined body folds to a constant where the design needs one, such as
    /// the bound of a loop that must unroll, which a call cannot give.
    pub(in crate::lowering) fn constant_arguments(
        &self,
        subroutine: &sv::ir::Subroutine,
        args: &[Option<sv::ir::Expr>],
    ) -> bool {
        // Without arguments the result depends on the body alone, which may
        // read state; a constant one is folded before the call is lowered.
        !subroutine.params.is_empty()
            && subroutine
                .params
                .iter()
                .enumerate()
                .all(|(position, param)| {
                    let Some(actual) = args
                        .get(position)
                        .and_then(Option::as_ref)
                        .or(param.default.as_ref())
                    else {
                        return false;
                    };
                    let mut calls = Vec::new();
                    collect_calls(actual, &mut calls);
                    let mut names = HashSet::default();
                    expr_idents(actual, &mut names);
                    calls.is_empty() && names.iter().all(|name| self.constants.contains_key(name))
                })
    }

    /// Whether `body` may call, directly or through inlined subroutines, a
    /// subroutine that goes out of line: a combinational process that does
    /// is lowered procedurally, since its symbolic form expands every call.
    pub(in crate::lowering) fn calls_out_of_line(&mut self, body: &[sv::ir::Stmt]) -> bool {
        if self.out_of_line().is_none() {
            return false;
        }
        let mut calls = Vec::new();
        for stmt in body {
            stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
        }
        let mut visited = HashSet::default();
        let mut seen_defaults = super::super::procedural::SeenDefaults::default();
        while let Some((callee, args)) = calls.pop() {
            let sv::ir::Callee::Subroutine(name) = callee else {
                continue;
            };
            // A call lowered as its constant value calls nothing.
            if args.is_empty() && self.constant_functions.value(&name).is_some() {
                continue;
            }
            let Some(subroutine) = self.subroutine(&name).cloned() else {
                continue;
            };
            // A static subroutine keeps its formals and result in module
            // variables, which a combinational process's call cannot share
            // with it (see `Ff::out_of_line_callee`).
            if subroutine.automatic
                && self.out_of_line_policy(&name)
                && self.out_of_line_reason(&name).is_none()
                && (self.out_of_line_whatever_arguments(&name)
                    || !self.constant_arguments(&subroutine, &args))
            {
                return true;
            }
            default_calls(&subroutine, &args, &mut calls, &mut seen_defaults);
            if visited.insert(name) {
                for stmt in &subroutine.body {
                    stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
                }
            }
        }
        false
    }
}
