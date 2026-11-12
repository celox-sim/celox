//! Declarations of a body: its typedefs, variables and parameters.
use super::*;

impl BodyBuilder<'_, '_, '_> {
    /// Declare typedef `id` of a block or subroutine body, visible to the
    /// rest of the scope (IEEE 1800-2023 6.18). A forward typedef declares
    /// nothing a body uses.
    pub(super) fn local_type_alias(&mut self, id: celox_sv_syntax::TypeAliasId) -> Converted<()> {
        let native = self.tree.declarations();
        let alias = native.type_alias(id);
        let celox_sv_syntax::TypeAliasTarget::DataType(target) = alias.target else {
            return Ok(());
        };
        // The members of an enumeration the typedef declares, collected and
        // checked as those of a typedef of the module (6.19); a typedef of
        // another enumeration declares none.
        let mut members = EnumMemberConstants::default();
        let aliases: SharedAliases = self.type_aliases.clone().into();
        let ty = types::bind_local_alias(
            self.tree,
            self.session,
            alias,
            &self.dims.const_env,
            &aliases,
            self.dims.type_declarations.as_ref(),
            &mut members,
        )?
        .ok_or_else(|| unsupported("local type declaration of this type"))?;
        drop(aliases);
        let name = instances::normalize_identifier(&alias.name.spelling);
        // The type hides a variable or constant of an enclosing scope with
        // its name.
        let hidden = self.take_binding(&name, None, 0);
        self.names
            .scopes
            .last_mut()
            .expect("a body always has a scope")
            .entries
            .push(hidden);
        let binding = parameters::enum_constant_type(
            self.tree,
            self.session,
            target,
            &ty,
            &self.dims.const_env,
            &self.type_aliases,
        )?;
        for (member, literal) in members.exprs {
            let r#type = members.types[&member];
            let entry = self.take_binding(&member, None, 0);
            if let Some(&number) = members.numbers.get(&member) {
                self.dims.const_env.insert(member.clone(), number);
            }
            self.dims.const_env.insert(enum_marker(&member), 1);
            insert_parameter_type_markers(&mut self.dims.const_env, &member, r#type);
            if let Expr::Literal(text) = &literal {
                self.names
                    .local_constants
                    .insert(member.clone(), text.clone());
            }
            self.dims.parameter_values.insert(member.clone(), literal);
            self.dims
                .expression_signedness
                .insert(member.clone(), r#type.signed);
            if let Some(binding) = &binding {
                Arc::make_mut(&mut self.dims.constant_types)
                    .insert(member.clone(), binding.clone());
            }
            self.names
                .scopes
                .last_mut()
                .expect("a body always has a scope")
                .entries
                .push(entry);
        }
        let shadowed = self.type_aliases.insert(name.clone(), ty.clone());
        self.dims.type_aliases.insert(name.clone(), ty);
        self.names
            .scopes
            .last_mut()
            .expect("a body always has a scope")
            .shadowed_types
            .push((name, shadowed));
        Ok(())
    }

    /// Declare native local variables and return their initializations.
    pub(in crate::ast) fn block_item_declaration(
        &mut self,
        native: &celox_sv_syntax::LocalDeclaration,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        let native = match native {
            celox_sv_syntax::LocalDeclaration::Parameters(id) => return self.local_parameters(*id),
            celox_sv_syntax::LocalDeclaration::Variables(value)
            | celox_sv_syntax::LocalDeclaration::Constructed(value) => value,
            celox_sv_syntax::LocalDeclaration::TypeAlias(id) => {
                self.local_type_alias(*id)?;
                return Ok(Vec::new());
            }
            celox_sv_syntax::LocalDeclaration::Unconverted(_) => {
                return Err(unsupported("let declaration inside a procedural block"));
            }
            _ => return Err(unsupported("local declaration")),
        };
        let signals = type_binding::local_signals(
            native,
            self.tree,
            self.session,
            &self.dims.const_env,
            &self.type_aliases,
            self.dims.type_declarations.as_ref(),
            None,
            Some(&self.dims),
        )?;
        let mut stmts = Vec::new();
        for (mut signal, assignment) in
            signals
                .into_iter()
                .zip(native.variables.iter().filter(|variable| {
                    type_binding::ordinary_variable(
                        variable,
                        native.data_type,
                        self.tree.declarations(),
                    )
                }))
        {
            if signal.r#type.unpacked_struct().is_some()
                && !signal.r#type.unpacked_ranges.is_empty()
            {
                if signal.is_const {
                    return Err(unsupported("const variable of unpacked structure type"));
                }
                signal.r#type = unpacked_structs::array_storage(signal.r#type)?;
            }
            // A declaration may not repeat a name of its own scope, such as
            // a subroutine argument (IEEE 1800-2023 6.21).
            let scope = self.names.scopes.last().expect("a body always has a scope");
            if scope
                .entries
                .iter()
                .any(|entry| entry.source == signal.name())
            {
                return Err(unsupported(format!(
                    "duplicate declaration of `{}` in one scope",
                    signal.name()
                )));
            }
            let automatic = match native.lifetime.as_deref() {
                Some("automatic") => true,
                Some("static") => false,
                _ => self.automatic,
            };
            let init = match assignment.value {
                Some(root) => {
                    let effects = self.effects(&[root])?;
                    // A static variable is initialized once, before any
                    // process starts (IEEE 1800-2023 6.21).
                    if !automatic && !effects.is_empty() {
                        return Err(unsupported(
                            "assignment, increment or decrement in a static variable initializer",
                        ));
                    }
                    stmts.extend(effects);
                    let target = LValue::Ident(signal.name().to_string());
                    let mut scoped = self.dims.clone();
                    scoped.insert(
                        signal.name().to_string(),
                        dimensions_from_type(signal.r#type()),
                    );
                    expression_binding::check_chandle_assignment(
                        signal.r#type().chandle && signal.r#type().unpacked_ranges.is_empty(),
                        root,
                        self.tree,
                        self.tree.declarations(),
                        &self.dims,
                        self.session,
                    )?;
                    let mut init = expression_binding::expression(
                        root,
                        self.tree.declarations(),
                        &target,
                        self.tree,
                        self.session,
                        &scoped,
                    )?;
                    self.rename_expr(&mut init);
                    Some(init)
                }
                // Each declaration of an event creates a synchronization
                // object (IEEE 1800-2023 15.5).
                None if events::is_event(signal.r#type()) => Some(events::new_event()),
                None if signal.r#type().kind() == TypeKind::Event => {
                    return Err(unsupported("array of named events"));
                }
                None => None,
            };
            let implicit_event = assignment.value.is_none() && init.is_some();
            if signal.r#type().unpacked_struct().is_some()
                && signal.r#type().unpacked_ranges.is_empty()
            {
                let explicit = native.lifetime.as_deref() == Some("static");
                stmts.extend(self.aggregate_local(
                    &signal,
                    assignment.value,
                    init,
                    automatic,
                    explicit,
                    assignment.tokens.start,
                )?);
                continue;
            }
            let name = if automatic {
                self.declare(signal.name(), signal.r#type().clone())
            } else {
                self.declare_static(
                    signal.name(),
                    signal.r#type().clone(),
                    assignment.tokens.start,
                )
            };
            if signal.is_const {
                let mut shape = self.dims.get(signal.name()).unwrap().clone();
                shape.randomizable = false;
                self.dims.insert(signal.name().to_string(), shape);
            }
            if !automatic {
                // A static event is created once, before any procedure runs,
                // whatever the procedure that declares it.
                if native.lifetime.as_deref() == Some("static") || implicit_event {
                    self.state.explicit_statics.insert(name.clone());
                }
                let r#type = scoped_type(signal.r#type().clone(), &self.dims.const_env);
                self.state.statics.push((name.clone(), r#type));
            }
            stmts.push(Stmt::Local { name, init });
        }
        Ok(stmts)
    }

    /// A local of unpacked structure type: its leaf variables, then the
    /// assignment of its initializer.
    pub(super) fn aggregate_local(
        &mut self,
        signal: &crate::ast::Signal,
        root: Option<celox_sv_syntax::ExprId>,
        init: Option<Expr>,
        automatic: bool,
        explicit: bool,
        declaration: usize,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        if signal.is_const {
            return Err(unsupported("const variable of unpacked structure type"));
        }
        if let Some(root) = root {
            let structure =
                unpacked_structs::type_id(signal.r#type(), self.tree, self.session, &self.dims)?;
            unpacked_structs::check_value(
                structure,
                root,
                self.tree.declarations(),
                self.tree,
                self.session,
                &self.dims,
            )?;
        }
        let leaves = self.declare_aggregate(
            signal.name(),
            signal.r#type(),
            (!automatic).then_some(declaration),
        )?;
        // A static variable is initialized once, not on each entry: each leaf
        // keeps its part of the initializer, which is hoisted with it.
        let aggregate = signal
            .r#type()
            .unpacked_struct()
            .expect("an unpacked structure local")
            .clone();
        let mut init = init;
        let mut parts = match init.take() {
            Some(value)
                if !automatic && unpacked_structs::leaf_parts(&aggregate, &value).is_some() =>
            {
                unpacked_structs::leaf_parts(&aggregate, &value)
                    .expect("checked by the guard")
                    .into_iter()
                    .map(Some)
                    .collect()
            }
            Some(_) if !automatic => {
                return Err(unsupported(
                    "initializer of a static variable of unpacked structure type that is not a pattern or variable",
                ));
            }
            None if root.is_none() => aggregate
                .leaves()
                .into_iter()
                .map(|leaf| leaf.default)
                .collect(),
            other => {
                init = other;
                vec![None; leaves.len()]
            }
        };
        let mut stmts = Vec::new();
        for ((name, r#type), part) in leaves.iter().zip(parts.drain(..)) {
            if !automatic {
                // An explicitly static member of a repeating process is
                // initialized before simulation, like a scalar.
                if explicit {
                    self.state.explicit_statics.insert(name.clone());
                }
                let r#type = scoped_type(r#type.clone(), &self.dims.const_env);
                self.state.statics.push((name.clone(), r#type));
            }
            stmts.push(Stmt::Local {
                name: name.clone(),
                init: part,
            });
        }
        if let Some(rhs) = init {
            stmts.push(Stmt::AssignConcat {
                parts: leaves
                    .into_iter()
                    .map(|(name, _)| LValue::Ident(name))
                    .collect(),
                rhs,
                nonblocking: false,
            });
        }
        Ok(stmts)
    }

    /// Declare a variable of unpacked structure type: hide the names of it
    /// and its members, then declare its leaf variables. Returns their unique
    /// names and types, first declared first.
    pub(in crate::ast) fn declare_aggregate(
        &mut self,
        source: &str,
        r#type: &Type,
        static_declaration: Option<usize>,
    ) -> Result<Vec<(String, Type)>, AnalyzerError> {
        if !r#type.unpacked_ranges.is_empty() {
            return Err(unsupported("array of unpacked structures"));
        }
        for (name, ty) in unpacked_structs::aggregates(source, r#type) {
            let shape = bound_dimensions_from_type(
                &ty,
                &self.tree.functions,
                &mut self.session.types,
                &self.dims.const_env,
            );
            let entry = self.take_binding(&name, None, 0);
            self.dims.insert(name.clone(), shape);
            self.names
                .scopes
                .last_mut()
                .expect("a body always has a scope")
                .entries
                .push(entry);
        }
        let aggregate = r#type
            .unpacked_struct()
            .expect("an unpacked structure type")
            .clone();
        Ok(aggregate
            .leaves()
            .into_iter()
            .map(|leaf| {
                let name = if let Some(declaration) = static_declaration {
                    self.declare_static(&leaf.name(source), leaf.r#type.clone(), declaration)
                } else {
                    self.declare(&leaf.name(source), leaf.r#type.clone())
                };
                (name, leaf.r#type)
            })
            .collect())
    }

    /// Make the parameters of a declaration in the body known as constants.
    pub(super) fn local_parameters(
        &mut self,
        id: celox_sv_syntax::ParameterDeclarationId,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        let mut parameters = Vec::new();
        let authored = self
            .tree
            .declarations()
            .parameter_declaration(id)
            .parameters
            .iter()
            .collect::<Vec<_>>();
        parameters::parameters_from_native(
            &authored,
            self.tree,
            self.session,
            &mut parameters,
            true,
            &self.dims.const_env,
            &self.type_aliases,
            &HashMap::default(),
        )?;
        let mut types = parameter_types_from_const_env(&self.dims.const_env);
        for parameter in &parameters {
            let name = parameter.name();
            let entry = self.take_binding(name, None, parameter.packed_ranges.len());
            types.remove(name);
            if !parameters::bind_parameter(
                self.tree,
                self.session,
                &mut self.dims.const_env,
                &mut types,
                &mut self.dims.parameter_values,
                parameter,
            ) {
                return Err(unsupported("local parameter whose value is not constant"));
            }
            let literal = if let Some(value) = self.dims.const_env.get(name).copied() {
                match types.get(name) {
                    Some(ty) => format_typed_parameter_literal(value, ty.width, ty.signed),
                    None => value.to_string(),
                }
            } else if let Some(Expr::Literal(value)) = self.dims.parameter_values.get(name) {
                value.clone()
            } else {
                return Err(unsupported("local parameter whose value is not constant"));
            };
            self.names
                .local_constants
                .insert(name.to_string(), literal.clone());
            self.dims
                .parameter_values
                .insert(name.to_string(), Expr::Literal(literal));
            if let Some(dimensions) =
                parameter_packed_dimensions(std::slice::from_ref(parameter)).remove(name)
            {
                self.dims.insert(name.to_string(), dimensions);
            }
            if let Some(ty) = &parameter.semantic_type {
                Arc::make_mut(&mut self.dims.constant_types).insert(name.into(), ty.clone());
            }
            if let Some(ty) = types.get(name) {
                self.dims
                    .expression_signedness
                    .insert(name.to_string(), ty.signed);
            }
            self.names
                .scopes
                .last_mut()
                .expect("a body always has a scope")
                .entries
                .push(entry);
        }
        Ok(Vec::new())
    }
}
