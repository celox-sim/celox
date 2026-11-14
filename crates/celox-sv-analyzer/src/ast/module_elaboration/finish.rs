//! The symbols a scope exports and the module it elaborates to.

use super::*;

impl ScopeElaboration<'_> {
    /// The symbols of a package, or of a scope that exports them, for the scopes
    /// that use it.
    pub(super) fn symbols(
        &mut self,
        syntax_tree: &AnalysisContext<'_>,
        session: &mut Session,
        inline_randomizations: Vec<classes::constraints::InlineRequest>,
    ) -> Option<scope::ScopeSymbols> {
        if self.is_package || self.export_symbols {
            Arc::make_mut(&mut self.packed_dimensions.constant_types).extend(
                classes::specializations::export_constant_types(syntax_tree, session),
            );
        }
        // A package keeps its symbols for the scopes that use it.
        (self.is_package || self.export_symbols).then(|| scope::ScopeSymbols {
            unit: None,
            package_scopes: self.imported.package_scopes.clone(),
            classes: classes::scope_classes(syntax_tree, session, &self.const_env)
                .into_iter()
                .map(Arc::new)
                .collect(),
            class_member_types: classes::specializations::export_member_types(syntax_tree, session),
            inline_randomizations,
            // A scope exports no other instance's subroutines.
            hierarchical_subroutines: Vec::new(),
            declarations_only: self.imported.declarations_only,
            type_parameter_bindings: HashMap::default(),
            pattern_parameter_bindings: HashMap::default(),
            const_env: (*self.const_env).clone(),
            parameter_values: self.parameter_values.clone(),
            parameters: self.parameters.clone(),
            // A class of the scope is a type of it (IEEE 1800-2023 8.3).
            type_aliases: self
                .type_aliases
                .iter()
                .map(|(name, ty)| (name.clone(), ty.clone()))
                .chain(classes::scope_class_types(syntax_tree))
                .collect(),
            // The types the scope declares, also those that have no
            // binding, for its importers (IEEE 1800-2023 26.3). Those it
            // imports are the exporting packages' own, which an import
            // or export makes visible by name.
            type_declarations: types::declared_type_names(syntax_tree).unwrap_or_default(),
            enum_constants: self.enum_constants.clone(),
            constant_types: (*self.packed_dimensions.constant_types).clone(),
            functions: self.functions.clone(),
            function_return_types: (*self.packed_dimensions.function_return_types).clone(),
            constant_functions: syntax_tree.functions.clone(),
            subroutine_params: self.subroutine_params.clone(),
            subroutine_shapes: self
                .subroutine_shapes
                .iter()
                .map(|(name, shapes)| (name.clone(), Arc::new(shapes.clone())))
                .collect(),
            subroutines: self.subroutines.iter().cloned().map(Arc::new).collect(),
            locals: self.locals.iter().cloned().map(Arc::new).collect(),
            dpi_imports: self.dpi_imports.clone(),
            // A package's signals are its array parameters and constant
            // variables, initialized by its initial processes.
            signals: self.signals.clone(),
            ports: Vec::new(),
            state_signals: Vec::new(),
            initial_processes: self.initial_processes.clone(),
            comb_processes: self.comb_processes.clone(),
            ff_processes: self.ff_processes.clone(),
            aliases: self.imported.aliases.clone(),
            interface_members: self.imported.interface_members.clone(),
            local_aliases: self.imported.local_aliases.clone(),
            interface_types: self.imported.interface_types.clone(),
            interface_modports: self.imported.interface_modports.clone(),
            interface_arrays: self.imported.interface_arrays.clone(),
            interface_scopes: self.imported.interface_scopes.clone(),
            interface_declarations: self.imported.interface_declarations.clone(),
            interface_instances: self.imported.interface_instances.clone(),
            generate_imports: HashMap::default(),
        })
    }

    /// Adds the subroutines, locals and DPI imports of the packages the scope
    /// uses. Returns the package parameters it sees.
    pub(super) fn lower_imported(&mut self) -> Vec<Parameter> {
        // The subroutines of the packages the module uses that it can call,
        // and their locals and DPI imports, are lowered with it.
        {
            let mut known: HashSet<&str> = self
                .subroutines
                .iter()
                .map(|subroutine| subroutine.name.as_str())
                .collect();
            let mut imported: HashMap<&str, &Subroutine> = HashMap::default();
            for subroutine in &self.imported.subroutines {
                if !known.contains(subroutine.name.as_str()) {
                    imported
                        .entry(subroutine.name.as_str())
                        .or_insert(subroutine);
                }
            }
            let reachable = self.reachable_imports(&imported);
            let missing = self
                .imported
                .subroutines
                .iter()
                .filter(|subroutine| {
                    reachable.contains(subroutine.name.as_str())
                        && known.insert(subroutine.name.as_str())
                })
                .map(|subroutine| (**subroutine).clone())
                .collect::<Vec<_>>();
            self.subroutines.extend(missing);
            let mut known: HashSet<&str> = self
                .locals
                .iter()
                .map(|local| local.name.as_str())
                .collect();
            let missing = self
                .imported
                .locals
                .iter()
                .filter(|local| known.insert(local.name.as_str()))
                .map(|local| (**local).clone())
                .collect::<Vec<_>>();
            self.locals.extend(missing);
        }
        for import in &self.imported.dpi_imports {
            if !self
                .dpi_imports
                .iter()
                .any(|known| known.name() == import.name())
            {
                self.dpi_imports.push(import.clone());
            }
        }
        let mut imported_parameters = self.imported.parameters.clone();
        imported_parameters.retain(|parameter| {
            !self
                .parameters
                .iter()
                .any(|local| local.name == parameter.name)
        });
        imported_parameters
    }

    /// Drops the bodies of the functions that declaration contracts cannot
    /// publish as one expression.
    pub(super) fn prune_symbolic_functions(&mut self, syntax_tree: &AnalysisContext<'_>) {
        // Expression construction tracks only whole-variable assignments.
        // Leave calls of functions with selected/concatenated writes symbolic
        // in declaration contracts rather than publishing a partial body.
        // Expression inlining also sizes formal and return conversions at the
        // default specialization; symbolic signature widths cannot be inlined.
        for subroutine in &self.declaration_subroutines {
            let mut selected_write = false;
            for statement in &subroutine.body {
                statement.walk(&mut |statement| {
                    selected_write |= matches!(
                        statement,
                        crate::procedural::StmtBase::Assign {
                            lhs: crate::ir::LValue::Select { .. },
                            ..
                        } | crate::procedural::StmtBase::AssignConcat { .. }
                    );
                });
            }
            let symbolic_type = subroutine
                .return_type
                .as_ref()
                .is_some_and(|ty| ty.has_symbolic_ranges(&syntax_tree.functions))
                || subroutine
                    .params
                    .iter()
                    .any(|param| param.r#type.has_symbolic_ranges(&syntax_tree.functions));
            if (selected_write || symbolic_type)
                && let Some(function) = self.functions.get_mut(&subroutine.name)
            {
                function.body = None;
            }
        }

        // A native default value is not evidence of a representable symbolic
        // body. Keep failed declaration lowerings visible to contract checking.
        for function in self.functions.values_mut() {
            if !self
                .declaration_subroutines
                .iter()
                .any(|subroutine| subroutine.name == function.name)
            {
                function.body = None;
            }
        }
    }

    /// A port actual that calls a subroutine, such as OpenTitan's
    /// `.mubi_i(mubi8_and_lo(init_locked, access_i.write_lock))`, is the value
    /// of the call in the instantiating module (IEEE 1800-2023 23.3.2.2): a
    /// variable of the function's result type that a continuous assignment
    /// of the module computes, under the instance's condition, which the
    /// port then takes. A connection reads no subroutine itself.
    ///
    /// It runs before the bodies are validated, which checks the calls of
    /// the continuous assignments as those of any other (13.4.1).
    pub(super) fn hoist_call_actuals(&mut self) {
        let mut taken = self
            .ports
            .iter()
            .map(|port| port.name.clone())
            .chain(self.signals.iter().map(|signal| signal.name.clone()))
            .chain(self.instances.iter().map(|instance| instance.name.clone()))
            .chain(
                self.parameters
                    .iter()
                    .chain(&self.imported.parameters)
                    .map(|parameter| parameter.name().to_string()),
            )
            .collect::<HashSet<_>>();
        // The result type of each function: as declared, for an aggregate
        // such as an unpacked array or a tagged union, whose declaration the
        // scope keeps; otherwise a vector of the result's width and kind.
        let result_types: HashMap<String, Type> = self
            .subroutines
            .iter()
            .chain(
                self.imported
                    .subroutines
                    .iter()
                    .map(|subroutine| &**subroutine),
            )
            .filter(|subroutine| !subroutine.is_task)
            .filter_map(|subroutine| {
                let declared = self
                    .functions
                    .get(&subroutine.name)
                    .or_else(|| self.imported.functions.get(&subroutine.name))
                    .and_then(|function| function.return_type.clone());
                let r#type =
                    declared.or_else(|| subroutine.return_type.as_ref().and_then(ast_type_of))?;
                Some((subroutine.name.clone(), r#type))
            })
            .collect();
        let component = |text: &str| {
            text.chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
                .collect::<String>()
        };
        let mut hoisted = 0usize;
        let mut signals = Vec::new();
        let mut processes = Vec::new();
        // An actual of an instance array is connected to each instance, or
        // each takes a part-select of it (IEEE 1800-2023 23.3.3.5, 28.3.6):
        // it names no element, so the elements share the one value of the
        // call, which the frontend distributes as any other expression.
        for instance in &mut self.instances {
            for connection in &mut instance.port_connections {
                let Some(actual) = connection.actual_expr.take() else {
                    continue;
                };
                let original = actual.clone();
                let (signals_before, processes_before) = (signals.len(), processes.len());
                let prefix = format!(
                    "{}_{}",
                    component(&instance.name),
                    component(&connection.formal)
                );
                // Each call in the actual, also inside an operator such as
                // `f(a) + 1`, is replaced by a variable holding its value.
                let mut hoist = |call: &Expr| -> Option<String> {
                    let Expr::Call {
                        callee: Callee::Subroutine(name),
                        ..
                    } = call
                    else {
                        return None;
                    };
                    let r#type = result_types.get(name)?;
                    // A name the module does not declare otherwise.
                    let hidden = loop {
                        let hidden = format!("__port_call{hoisted}_{prefix}");
                        hoisted += 1;
                        if taken.insert(hidden.clone()) {
                            break hidden;
                        }
                    };
                    signals.push(Signal::new(hidden.clone(), r#type.clone()));
                    processes.push(CombProcess::new(
                        CombProcessKind::ContinuousAssign,
                        instance.condition.clone(),
                        vec![Assignment::new(LValue::Ident(hidden.clone()), call.clone())],
                    ));
                    Some(hidden)
                };
                let rewritten = hoist_calls(actual, &mut hoist);
                // The actual reevaluates when any operand changes, calling
                // the function again (IEEE 1800-2023 10.3.2, 23.3.3): a call
                // inside an operator is hoisted only when it is the only one
                // and the rest of the actual reads no variable, so that only
                // the call's own operands trigger it. Otherwise the actual
                // stays.
                let hidden: HashSet<&str> = signals[signals_before..]
                    .iter()
                    .map(|signal: &Signal| signal.name.as_str())
                    .collect();
                // Two calls would also each reevaluate the other: one call
                // only, whose operands are all the actual's.
                if matches!(&rewritten, Expr::Ident(_))
                    || hidden.len() == 1 && reads_no_variable(&rewritten, &hidden)
                {
                    connection.actual_expr = Some(rewritten);
                } else {
                    signals.truncate(signals_before);
                    processes.truncate(processes_before);
                    connection.actual_expr = Some(original);
                }
            }
        }
        self.signals.extend(signals);
        self.comb_processes.extend(processes);
    }

    /// The module the scope elaborated to.
    pub(super) fn finish(
        self,
        syntax_tree: &AnalysisContext<'_>,
        session: &mut Session,
        assignments: Vec<Assignment>,
        symbols: Option<scope::ScopeSymbols>,
        imported_parameters: Vec<Parameter>,
    ) -> Module {
        let mut interface_scope_names = self
            .imported
            .interface_types
            .keys()
            .chain(self.imported.interface_arrays.keys())
            .cloned()
            .collect::<Vec<_>>();
        interface_scope_names.sort_unstable();
        interface_scope_names.dedup();
        let interface_handles = virtual_interfaces::handles(session, self.imported, &self.ports);

        Module {
            name: self.name,
            program: self.program,
            interface_scope_names,
            interface_handles,
            parameters: self.parameters,
            ports: self.ports,
            signals: self.signals,
            instances: self.instances,
            assignments,
            comb_processes: self.comb_processes,
            ff_processes: self.ff_processes,
            initial_processes: self.initial_processes,
            locals: self.locals,
            subroutines: self.subroutines,
            dpi_imports: self.dpi_imports,
            constant_functions: syntax_tree.functions.clone(),
            symbolic_functions: self.functions,
            declaration_subroutines: self.declaration_subroutines,
            imported_parameters,
            symbols,
        }
    }
}

/// `expr` with each outermost subexpression that `hoist` takes replaced by
/// the variable it names. A call that the expression evaluates only under a
/// condition, in a branch of a conditional operator or the right operand of
/// a short-circuit operator (IEEE 1800-2023 11.4.7, 11.4.11), stays: its
/// variable would evaluate it unconditionally.
fn hoist_calls(expr: Expr, hoist: &mut dyn FnMut(&Expr) -> Option<String>) -> Expr {
    if let Some(hidden) = hoist(&expr) {
        return Expr::Ident(hidden);
    }
    match expr {
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(hoist_calls(*condition, hoist)),
            then_expr,
            else_expr,
        },
        Expr::Binary {
            left,
            op: op @ (BinaryOp::LogicAnd | BinaryOp::LogicOr),
            right,
        } => Expr::Binary {
            left: Box::new(hoist_calls(*left, hoist)),
            op,
            right,
        },
        expr => virtual_interfaces::map_children(expr, &mut |child| hoist_calls(child, hoist)),
    }
}

/// Whether `expr` reads no variable but those `hidden` names, and calls
/// nothing: its value changes only with theirs.
fn reads_no_variable(expr: &Expr, hidden: &HashSet<&str>) -> bool {
    match expr {
        Expr::Ident(name) => hidden.contains(name.as_str()),
        Expr::Call { .. } => false,
        Expr::Select { expr, msb, lsb, .. } => {
            constant_bound(msb) && constant_bound(lsb) && reads_no_variable(expr, hidden)
        }
        _ => virtual_interfaces::children(expr)
            .into_iter()
            .all(|child| reads_no_variable(child, hidden)),
    }
}

/// Whether select bound `bound` names nothing: no variable, no call.
fn constant_bound(bound: &ConstExpr) -> bool {
    match bound {
        ConstExpr::Literal(_) => true,
        ConstExpr::Ident(_) | ConstExpr::Function { .. } => false,
        ConstExpr::Select { expr, bit } => constant_bound(expr) && constant_bound(bit),
        ConstExpr::SelectRange { expr, msb, lsb, .. } => {
            constant_bound(expr) && constant_bound(msb) && constant_bound(lsb)
        }
        ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => constant_bound(expr),
        ConstExpr::Binary { left, right, .. } => constant_bound(left) && constant_bound(right),
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => constant_bound(condition) && constant_bound(then_expr) && constant_bound(else_expr),
    }
}

/// The declared type of a result of IR type `r#type`: its declaration's,
/// or a vector of its width, kind and sign.
fn ast_type_of(r#type: &crate::ir::Type) -> Option<Type> {
    if let Some(container) = r#type.container() {
        return Some(container.clone());
    }
    if !r#type.unpacked_ranges().is_empty() || r#type.class().is_some() {
        return None;
    }
    let kind = match r#type.kind() {
        crate::ir::TypeKind::Real => return type_binding::builtin_type("real", None),
        crate::ir::TypeKind::Shortreal => return type_binding::builtin_type("shortreal", None),
        crate::ir::TypeKind::String => return type_binding::builtin_type("string", None),
        crate::ir::TypeKind::Bit => TypeKind::Bit,
        crate::ir::TypeKind::Logic | crate::ir::TypeKind::Implicit => TypeKind::Logic,
        crate::ir::TypeKind::Reg => TypeKind::Reg,
        _ => return None,
    };
    let width = r#type.resolved_width()?;
    let mut ast = Type::new(kind);
    ast.is_signed = r#type.is_signed();
    if width > 1 || !r#type.packed_ranges().is_empty() {
        ast.packed_ranges = vec![PackedRange::new(
            ConstExpr::Literal((width - 1).to_string()),
            ConstExpr::Literal("0".to_string()),
        )];
    }
    Some(ast)
}
