//! The subroutines and processes of a scope.

use super::*;

impl ScopeElaboration<'_> {
    /// The subroutines and processes of the scope, with net and continuous
    /// assignment delays lowered, and the static locals they keep as signals of
    /// the scope. Returns the inline randomizations they request.
    pub(super) fn bodies(
        &mut self,
        syntax_tree: &AnalysisContext<'_>,
        session: &mut Session,
        array_initial_processes: Vec<InitialProcess>,
    ) -> Result<Vec<classes::constraints::InlineRequest>, AnalyzerError> {
        self.locals = Vec::new();
        let mut local_counter = procedural::first_local(syntax_tree, session);
        let mut body_state = procedural::BodyState {
            locals: &mut self.locals,
            counter: &mut local_counter,
            subroutine_params: &self.subroutine_params,
            subroutine_directions: &self.subroutine_directions,
            automatic: lifetimes::automatic_by_default(syntax_tree),
            statics: Vec::new(),
            explicit_statics: HashSet::default(),
            static_initializers: Vec::new(),
            constant_evaluation: false,
        };
        (
            self.subroutines,
            self.comb_processes,
            self.ff_processes,
            self.initial_processes,
        ) = if self.imported.declarations_only {
            (Vec::new(), Vec::new(), Vec::new(), Vec::new())
        } else {
            let subroutines = procedural::subroutines_from_scope(
                syntax_tree,
                session,
                &self.const_env,
                &self.packed_dimensions,
                &self.parameter_values,
                &mut body_state,
            )?;
            let mut comb_processes = comb_processes_from_scope(
                syntax_tree,
                session,
                &self.const_env,
                &self.packed_dimensions,
                &self.parameter_values,
                &mut body_state,
            )?
            .into_iter()
            .map(|process| {
                substitute_process_constants_with_parameter_literals(
                    syntax_tree,
                    session,
                    process,
                    &self.const_env,
                    &self.parameter_values,
                )
            })
            .collect::<Vec<_>>();
            let mut ff_processes = ff_processes_from_scope(
                syntax_tree,
                session,
                &self.const_env,
                &self.parameter_values,
                &self.packed_dimensions,
                &mut body_state,
            )?;
            comb_processes.extend(self.imported.comb_processes.iter().cloned());
            ff_processes.extend(self.imported.ff_processes.iter().cloned());
            let mut initial_processes = array_initial_processes;
            initial_processes.extend(self.imported.initial_processes.iter().cloned());
            initial_processes.extend(procedural::initial_processes_from_scope(
                syntax_tree,
                session,
                &self.const_env,
                &self.packed_dimensions,
                &self.parameter_values,
                &mut body_state,
            )?);
            (subroutines, comb_processes, ff_processes, initial_processes)
        };
        net_delays::lower(
            syntax_tree,
            &mut self.packed_dimensions,
            &self.ports,
            &self.instances,
            &mut self.signals,
            &mut self.comb_processes,
        )?;
        net_drives::lower(
            &self.ports,
            &self.instances,
            &self.signals,
            &mut self.comb_processes,
        );
        continuous_delays::lower(
            syntax_tree,
            &self.packed_dimensions,
            &mut self.signals,
            &mut self.comb_processes,
            &mut self.initial_processes,
        )?;
        // Every module of a design with assertion control tasks shares
        // their state; the subroutines of a package do not check it.
        if session.controls_assertions
            && syntax_tree.package.is_none()
            && !self
                .signals
                .iter()
                .any(|signal| signal.name() == assertion_control::OFF)
        {
            self.signals.push(assertion_control::signal());
        }
        let forced = procedural_forces::lower(
            syntax_tree,
            &self.packed_dimensions,
            &self.ports,
            &mut self.signals,
            &self.comb_processes,
            &self.ff_processes,
            &mut self.initial_processes,
            &mut self.subroutines,
            &self.imported.external_forces.clone(),
        )?;
        procedural_assigns::lower(
            &self.packed_dimensions,
            &self.ports,
            &mut self.signals,
            &forced,
            &self.comb_processes,
            &mut self.ff_processes,
            &mut self.initial_processes,
            &mut self.subroutines,
            &self.subroutine_directions,
        )?;
        let (inline_subroutines, inline_randomizations) =
            procedural::randomization::inline_subroutines(
                syntax_tree,
                session,
                &mut body_state,
                &self.packed_dimensions,
            )?;
        self.subroutines.extend(inline_subroutines);
        // Static locals of the processes that keep their value are variables
        // of the module, initialized once with the declaration initializers.
        let statics: HashMap<String, Type> = body_state.statics.drain(..).collect();
        let (mut kept, mut static_initializers) = lifetimes::keep_static_locals(
            syntax_tree,
            session,
            &statics,
            &self.subroutine_directions,
            self.comb_processes
                .iter_mut()
                .map(|process| (&mut process.body, false))
                .chain(
                    self.ff_processes
                        .iter_mut()
                        .map(|process| (&mut process.body, false)),
                )
                .chain(
                    self.initial_processes
                        .iter_mut()
                        .map(|process| (&mut process.body, !process.repeats)),
                ),
            &body_state.explicit_statics,
            &[],
            false,
        )?;
        for subroutine in &self.subroutines {
            kept.extend(subroutine.persistent_locals.iter().cloned());
        }
        // Statics of subroutines are initialized first: a run-time
        // initializer of a process local may call the subroutine.
        static_initializers.splice(0..0, body_state.static_initializers.drain(..));
        {
            let kept: HashSet<&str> = kept.iter().map(String::as_str).collect();
            self.locals
                .retain(|local| !kept.contains(local.name.as_str()));
        }
        // The package or compilation unit this scope is, whose variables
        // are its own, not imported.
        let own_package = syntax_tree
            .root_unit()
            .ok()
            .filter(|unit| unit.kind == celox_sv_syntax::UnitKind::Package)
            .map(|unit| instances::normalize_identifier(&unit.name.spelling))
            .or_else(|| {
                (syntax_tree.scope_handle() == crate::source_tree::ScopeHandle::Compilation)
                    .then(|| packages::UNIT.to_string())
            });
        for name in kept {
            // A static variable is named by its owner and declaration (see
            // `BodyBuilder::declare_static`), so a scope that lowers a
            // package subroutine again, such as a method of a class
            // specialization an earlier analysis made, may import the
            // same variable. Another package's is that variable; one of
            // this scope's own package is this declaration.
            if let Some(index) = self.signals.iter().position(|signal| signal.name == name) {
                let own = self.signals[index]
                    .package_variable
                    .as_ref()
                    .is_some_and(|(package, _)| Some(package) == own_package.as_ref());
                if !own {
                    continue;
                }
                self.signals.remove(index);
            }
            self.signals
                .push(Signal::new(name.clone(), statics[&name].clone()));
        }
        if !static_initializers.is_empty() {
            // Declaration initializers run before the other initial blocks.
            let first_block = self
                .initial_processes
                .iter()
                .position(|process| !process.is_initializer())
                .unwrap_or(self.initial_processes.len());
            // They may read module variables, so they also stay after those
            // initializers when processes are reordered by creation.
            let creation_order = self
                .initial_processes
                .iter()
                .filter(|process| process.is_initializer())
                .map(|process| process.creation_order)
                .max()
                .unwrap_or(0);
            self.initial_processes.insert(
                first_block,
                InitialProcess {
                    creation_order,
                    site: None,
                    condition: None,
                    body: static_initializers,
                    kind: celox_design::ProcessKind::Initializer,
                    validation_body: Vec::new(),
                    synthetic: false,
                    repeats: false,
                },
            );
        }
        Ok(inline_randomizations)
    }

    /// Binds the calls of names that imports bind to the package subroutines and
    /// DPI imports they name.
    pub(super) fn rename_imported_calls(&mut self) {
        // A call of a name an import binds calls the package subroutine or
        // DPI import. Calls of subroutines declared in a generate block were
        // already bound to those.
        let calls: HashMap<String, String> = self
            .imported
            .aliases
            .iter()
            .filter(|(_, target)| {
                self.imported
                    .subroutines
                    .iter()
                    .any(|subroutine| subroutine.name == **target)
                    || self
                        .imported
                        .dpi_imports
                        .iter()
                        .any(|import| import.name() == target.as_str())
            })
            .map(|(name, target)| (name.clone(), target.clone()))
            .collect();
        if !calls.is_empty() {
            let rename = scope::Renamer { names: &calls };
            let body = |body: Vec<Stmt>| body.into_iter().map(|stmt| rename.stmt(stmt)).collect();
            for process in &mut self.comb_processes {
                process.body = body(std::mem::take(&mut process.body));
                process.sensitivity_body = body(std::mem::take(&mut process.sensitivity_body));
                for assignment in &mut process.assignments {
                    assignment.rhs = rename.expr(assignment.rhs.clone());
                }
            }
            for process in &mut self.ff_processes {
                process.body = body(std::mem::take(&mut process.body));
            }
            for process in &mut self.initial_processes {
                process.body = body(std::mem::take(&mut process.body));
                process.validation_body = body(std::mem::take(&mut process.validation_body));
            }
            for subroutine in &mut self.subroutines {
                *subroutine = rename.subroutine(subroutine.clone());
            }
            for instance in &mut self.instances {
                for connection in &mut instance.port_connections {
                    connection.map_expressions(|expr| rename.expr(expr));
                }
            }
        }
    }

    /// Checks the references of the bodies and classifies the processes that call
    /// timed tasks.
    pub(super) fn validate_bodies(&mut self) -> Result<(), AnalyzerError> {
        let nets = self
            .signals
            .iter()
            .filter(|signal| signal.is_net())
            .map(Signal::name)
            .chain(self.ports.iter().filter(|port| port.is_net).map(Port::name))
            .collect();
        procedural::references::validate(
            self.subroutines.iter().chain(
                self.imported
                    .subroutines
                    .iter()
                    .map(|subroutine| &**subroutine),
            ),
            &self.comb_processes,
            &self.ff_processes,
            &self.initial_processes,
            &self.subroutine_directions,
            &self.subroutine_params,
            &nets,
        )?;
        procedural::timed_tasks::classify(
            self.subroutines.iter().chain(
                self.imported
                    .subroutines
                    .iter()
                    .map(|subroutine| &**subroutine),
            ),
            &mut self.comb_processes,
            &mut self.ff_processes,
            &mut self.initial_processes,
        )?;
        Ok(())
    }

    /// The continuous assignments of the scope.
    pub(super) fn assignments(&self) -> Vec<Assignment> {
        self.comb_processes
            .iter()
            .flat_map(|process| process.assignments().iter().cloned())
            .collect()
    }
}
