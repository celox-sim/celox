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
        // The declarations of the covergroups with `ref` formals (IEEE
        // 1800-2023 19.3).
        let mut reference_sites = Vec::new();
        let mut body_state = procedural::BodyState {
            locals: &mut self.locals,
            counter: &mut local_counter,
            subroutine_params: &self.subroutine_params,
            subroutine_directions: &self.subroutine_directions,
            automatic: lifetimes::automatic_by_default(syntax_tree),
            statics: Vec::new(),
            explicit_statics: HashSet::default(),
            subroutine_statics: HashMap::default(),
            static_initializers: Vec::new(),
            constant_evaluation: false,
            assertion_callees: Vec::new(),
            disables: Default::default(),
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
            comb_processes.extend(
                port_connections(&self.ports, syntax_tree, session, &self.packed_dimensions)?
                    .into_iter()
                    .map(|process| {
                        substitute_process_constants_with_parameter_literals(
                            syntax_tree,
                            session,
                            process,
                            &self.const_env,
                            &self.parameter_values,
                        )
                    }),
            );
            let mut ff_processes = ff_processes_from_scope(
                syntax_tree,
                session,
                &self.const_env,
                &self.parameter_values,
                &self.packed_dimensions,
                &mut body_state,
            )?;
            let (scope_processes, sites) = procedural::initial_processes_from_scope(
                syntax_tree,
                session,
                &self.const_env,
                &self.packed_dimensions,
                &self.parameter_values,
                &mut body_state,
            )?;
            // The scope's own code, before the processes of the interface
            // instances it binds join it.
            covergroups::reject_outside_names(
                session,
                &subroutines,
                &comb_processes,
                &ff_processes,
                &scope_processes,
            )?;
            comb_processes.extend(self.imported.comb_processes.iter().cloned());
            ff_processes.extend(self.imported.ff_processes.iter().cloned());
            let mut initial_processes = array_initial_processes;
            initial_processes.extend(self.imported.initial_processes.iter().cloned());
            initial_processes.extend(scope_processes);
            // The variables that hold the covergroup instances sampled at
            // their clocking events (IEEE 1800-2023 19.3).
            self.signals.extend(
                sites
                    .signals
                    .into_iter()
                    .map(|(name, r#type)| Signal::new(name, r#type)),
            );
            ff_processes.extend(sites.edge_procedures);
            reference_sites = sites.references;
            (subroutines, comb_processes, ff_processes, initial_processes)
        };
        // The blocks a `disable` ends from outside them keep their process
        // in variables of the scope (IEEE 1800-2023 9.6.2); a `disable` of
        // any other name outside its block stays unsupported.
        let mut disables = std::mem::take(&mut body_state.disables);
        // A `disable` in an unselected generate branch elaborates to
        // nothing: take the branch off a block only it names.
        procedural::external_disables::unwrap_without_elaborated_target(
            &mut disables,
            self.comb_processes
                .iter_mut()
                .map(|process| &mut process.body)
                .chain(
                    self.ff_processes
                        .iter_mut()
                        .map(|process| &mut process.body),
                )
                .chain(
                    self.initial_processes
                        .iter_mut()
                        .map(|process| &mut process.body),
                ),
        );
        // A subroutine a branch calls may need the process that runs the
        // block, which only the lowered subroutines show (9.6.1, 9.6.3).
        let (needs_their_process, spawns_detached, randomizes) =
            procedural::external_disables::subroutine_process_effects(
                self.subroutines.iter().chain(
                    self.imported
                        .subroutines
                        .iter()
                        .map(|subroutine| &**subroutine),
                ),
            );
        if disables
            .targets
            .iter()
            .any(|target| disables.ambiguous.contains(target) || !disables.blocks.contains(target))
            || procedural::external_disables::calls_subroutine_that_needs_its_process(
                &disables,
                &needs_their_process,
            )
            || procedural::external_disables::pending_children_may_precede(
                &disables,
                self.comb_processes
                    .iter()
                    .map(|process| process.body.as_slice())
                    .chain(
                        self.ff_processes
                            .iter()
                            .map(|process| process.body.as_slice()),
                    )
                    .chain(
                        self.initial_processes
                            .iter()
                            .map(|process| process.body.as_slice()),
                    ),
                &spawns_detached,
            )
            || procedural::external_disables::own_process_may_be_named(
                &disables,
                self.comb_processes
                    .iter()
                    .map(|process| process.body.as_slice())
                    .chain(
                        self.ff_processes
                            .iter()
                            .map(|process| process.body.as_slice()),
                    )
                    .chain(
                        self.initial_processes
                            .iter()
                            .map(|process| process.body.as_slice()),
                    ),
                &needs_their_process,
            )
            || procedural::external_disables::random_state_may_change(
                &disables,
                self.comb_processes
                    .iter()
                    .map(|process| process.body.as_slice())
                    .chain(
                        self.ff_processes
                            .iter()
                            .map(|process| process.body.as_slice()),
                    )
                    .chain(
                        self.initial_processes
                            .iter()
                            .map(|process| process.body.as_slice()),
                    ),
                &randomizes,
            )
        {
            return Err(unsupported("disable statement"));
        }
        let mut blocks: Vec<_> = disables.blocks.into_iter().collect();
        blocks.sort_unstable();
        for label in blocks {
            self.signals.extend(procedural::external_disables::signals(
                &label,
                &mut session.types,
            ));
        }
        net_delays::lower(
            syntax_tree,
            &session.types,
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
            &session.types,
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
            session,
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
            &mut session.types,
            &syntax_tree.functions,
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
        // The subroutines of the covergroup types the scope declares
        // (IEEE 1800-2023 19.3): all of them where another scope may use
        // the type, else those the bodies above use.
        if !self.imported.declarations_only {
            self.subroutines
                .extend(procedural::covergroup_subroutines::scope_subroutines(
                    syntax_tree,
                    session,
                    &self.const_env,
                    &self.packed_dimensions,
                    &self.parameter_values,
                    &mut body_state,
                    self.is_package || self.export_symbols,
                )?);
            pass_covergroup_references(
                syntax_tree,
                session,
                &reference_sites,
                Bodies {
                    subroutines: &mut self.subroutines,
                    comb: &mut self.comb_processes,
                    ff: &mut self.ff_processes,
                    initial: &mut self.initial_processes,
                },
                &self.signals,
                &self.ports,
                &self.subroutine_directions,
            )?;
        }
        // The functions of UVM's C library that Celox provides report
        // through the UVM package (see `procedural::dpi_reports`).
        procedural::dpi_reports::report_through_uvm(
            syntax_tree,
            session,
            &mut body_state,
            &self.packed_dimensions,
            &mut self.dpi_imports,
            &mut self.subroutines,
        )?;
        self.assertion_callees = std::mem::take(&mut body_state.assertion_callees);
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
                    assertion_actions: false,
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
            for callee in &mut self.assertion_callees {
                *callee = rename.name(callee);
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
        procedural::references::validate_const_ref_ports(
            &self.name,
            &self.ports,
            self.subroutines.iter(),
            &self.comb_processes,
            &self.ff_processes,
            &self.initial_processes,
            &self.subroutine_directions,
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
            &self.assertion_callees,
            &self
                .dpi_imports
                .iter()
                .chain(&self.imported.dpi_imports)
                .map(crate::ir::DpiImport::name)
                .collect(),
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

/// The subroutines and processes of a scope.
struct Bodies<'a> {
    subroutines: &'a mut Vec<Subroutine>,
    comb: &'a mut Vec<CombProcess>,
    ff: &'a mut Vec<FfProcess>,
    initial: &'a mut Vec<InitialProcess>,
}

/// `sample()` of the covergroups with `ref` formals through the variables
/// of `sites`, the declarations of the scope that construct their
/// instances, gets the actuals of the declaration in `bodies`, no such
/// actual is a net of `signals` or `ports`, and nothing else writes such a
/// variable (IEEE 1800-2023 13.5.2, 19.3; see
/// `procedural::covergroup_sites::pass_references`). A scope without such a
/// covergroup type looks at nothing.
#[allow(clippy::too_many_arguments)]
fn pass_covergroup_references(
    syntax_tree: &AnalysisContext<'_>,
    session: &Session,
    sites: &[procedural::covergroup_sites::ReferenceSite],
    bodies: Bodies<'_>,
    signals: &[Signal],
    ports: &[Port],
    directions: &lifetimes::Directions,
) -> Result<(), AnalyzerError> {
    let references: HashMap<String, usize> = session
        .covergroups
        .values()
        .chain(&scope::imported(syntax_tree).covergroups)
        .filter(|model| model.has_references())
        .map(|model| (model.sample_subroutine(), model.references().count()))
        .collect();
    if references.is_empty() {
        return Ok(());
    }
    let nets = signals
        .iter()
        .filter(|signal| signal.is_net())
        .map(Signal::name)
        .chain(ports.iter().filter(|port| port.is_net).map(Port::name))
        .collect();
    procedural::covergroup_sites::check_site_actuals(sites, &nets)?;
    let Bodies {
        subroutines,
        comb,
        ff,
        initial,
    } = bodies;
    procedural::covergroup_sites::pass_references(
        sites,
        &|name| references.get(name).copied(),
        subroutines
            .iter_mut()
            .map(|subroutine| &mut subroutine.body)
            .chain(
                comb.iter_mut()
                    .flat_map(|process| [&mut process.body, &mut process.sensitivity_body]),
            )
            .chain(ff.iter_mut().map(|process| &mut process.body))
            .chain(
                initial
                    .iter_mut()
                    .flat_map(|process| [&mut process.body, &mut process.validation_body]),
            ),
    )?;
    procedural::covergroup_sites::check_site_variables(
        sites,
        subroutines.iter(),
        comb,
        ff,
        initial,
        directions,
    )
}
