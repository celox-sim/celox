//! Assignment statements: the value, the target and the form of each write.
use super::*;

impl BodyBuilder<'_, '_, '_> {
    /// The parts of a concatenated assignment target, most significant first,
    /// before renaming.
    pub(super) fn concat_lvalue(
        &mut self,
        root: celox_sv_syntax::ExprId,
    ) -> Converted<Option<Vec<(celox_sv_syntax::ExprId, LValue)>>> {
        let native = self.tree.declarations();
        let celox_sv_syntax::ExprKind::Concatenation(parts) = &native.expression(root).kind else {
            return Ok(None);
        };
        let mut pending = parts.iter().rev().copied().collect::<Vec<_>>();
        let mut parts = Vec::new();
        while let Some(id) = pending.pop() {
            if let celox_sv_syntax::ExprKind::Concatenation(items) = &native.expression(id).kind {
                pending.extend(items.iter().rev().copied());
            } else {
                if expression_binding::member_context(
                    id,
                    native,
                    self.tree,
                    self.session,
                    &self.dims,
                )?
                .is_some_and(|(guards, _)| !guards.is_empty())
                {
                    return Err(unsupported(
                        "concatenation assignment to a tagged union member",
                    ));
                }
                self.reject_tagged_integral_target(id)?;
                let target = expression_binding::variable_lvalue(
                    id,
                    native,
                    self.tree,
                    self.session,
                    &self.dims,
                    false,
                )?;
                virtual_interfaces::check_target(&target)?;
                parts.push((id, target));
            }
        }
        Ok(Some(parts))
    }

    pub(super) fn assignment_value(
        &mut self,
        op: &str,
        nonblocking: bool,
        native: (celox_sv_syntax::ExprId, celox_sv_syntax::ExprId),
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        self.assignment_value_with_deferred_checks(op, nonblocking, native, None)
    }

    /// Separate destination access checks when an intra-assignment control
    /// delays evaluation of the LHS. RHS access checks and conversion stay
    /// attached to the value evaluated before suspension.
    pub(super) fn assignment_value_with_deferred_checks(
        &mut self,
        op: &str,
        nonblocking: bool,
        native: (celox_sv_syntax::ExprId, celox_sv_syntax::ExprId),
        deferred_destination_checks: Option<&mut Vec<Expr>>,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        let stmts =
            self.assignment_statements(op, nonblocking, native, deferred_destination_checks)?;
        reject_escaped_places(&stmts)?;
        Ok(stmts)
    }

    pub(super) fn assignment_statements(
        &mut self,
        op: &str,
        nonblocking: bool,
        native: (celox_sv_syntax::ExprId, celox_sv_syntax::ExprId),
        deferred_destination_checks: Option<&mut Vec<Expr>>,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        let (left, right) = native;
        if op != "="
            && unpacked_structs::value_struct(
                left,
                self.tree.declarations(),
                self.tree,
                self.session,
                &self.dims,
            )
            .is_some()
        {
            return Err(unsupported(format!(
                "operator `{op}` on an unpacked structure"
            )));
        }
        if containers::element_member(
            left,
            self.tree.declarations(),
            self.tree,
            self.session,
            &self.dims,
        )?
        .is_some()
        {
            return Err(unsupported(
                "assignment to a member of a dynamic container element",
            ));
        }
        let declarations = self.tree.declarations();
        // A chandle is only assigned (IEEE 1800-2023 6.14); the destinations
        // dispatched below write without `variable_lvalue`, so its target
        // check runs here.
        expression_binding::check_chandle_target(
            op == "=",
            left,
            self.tree,
            declarations,
            &self.dims,
            self.session,
        )?;
        let chandle = expression_binding::native_chandle(
            self.tree,
            left,
            declarations,
            &self.dims,
            self.session,
        );
        expression_binding::check_chandle_assignment(
            chandle,
            right,
            self.tree,
            declarations,
            &self.dims,
            self.session,
        )?;
        // Every write to an enumerated place, whatever kind of place it
        // is, is judged before its kind writes it (6.19.3, 6.19.4).
        if op == "=" {
            assignments::check_enum_target(
                self.tree,
                self.session,
                left,
                right,
                self.tree.declarations(),
                &self.dims,
            )?;
        } else {
            assignments::check_enum_update(
                self.tree,
                self.session,
                left,
                self.tree.declarations(),
                &self.dims,
                op,
            )?;
        }
        if let Some(mut stmt) = containers::set(
            left,
            right,
            op,
            nonblocking,
            self.tree,
            self.session,
            &self.dims,
        )? {
            if let Stmt::Eval(expr) = &mut stmt {
                self.rename_expr(expr);
            }
            return Ok(vec![stmt]);
        }
        if let Some(stmts) = self.stream_assignment(op, nonblocking, left, right)? {
            return Ok(stmts);
        }
        if let Some(parts) = self.concat_lvalue(left)? {
            let places = parts
                .iter()
                .any(|(_, part)| matches!(part, LValue::Object { .. }));
            if op != "=" && places {
                return Err(unsupported("compound assignment to a concatenation"));
            }
            if places {
                let targets = parts.into_iter().map(|(id, _)| id).collect();
                return self.concat_place_assignment(left, targets, right, nonblocking);
            }
            if op != "=" {
                for (id, part) in &parts {
                    // Each part is updated as a target of its own (IEEE
                    // 1800-2023 11.4.1), an enumerated one as one too
                    // (6.19.3, 6.19.4).
                    assignments::check_enum_update(
                        self.tree,
                        self.session,
                        *id,
                        self.tree.declarations(),
                        &self.dims,
                        op,
                    )?;
                    // The parts are read and then written: a select with an
                    // effectful call, a subroutine or a random function,
                    // would choose them twice.
                    if let LValue::Select { msb, lsb, .. } = part
                        && (has_effectful_call(msb) || has_effectful_call(lsb))
                    {
                        return Err(unsupported(
                            "compound assignment to a concatenation whose select has an effectful call",
                        ));
                    }
                }
            }
            let mut parts = parts.into_iter().map(|(_, part)| part).collect::<Vec<_>>();
            for part in &mut parts {
                self.rename_lvalue(part);
            }
            let value = self.native_value(right)?;
            // `{a, b} op= v` is `{a, b} = {a, b} op v` (IEEE 1800-2023
            // 11.4.1): the concatenation reads its parts (11.4.12).
            let rhs = if op == "=" {
                value
            } else {
                let read = Expr::Concat(
                    parts
                        .iter()
                        .map(|part| statements::expr_from_lvalue(self.tree, part, &self.dims))
                        .collect(),
                );
                statements::assignment_op_value(
                    self.tree,
                    self.session,
                    read,
                    op,
                    value,
                    &self.dims,
                )?
            };
            return Ok(vec![Stmt::AssignConcat {
                parts,
                rhs,
                nonblocking,
            }]);
        }
        if op == "="
            && let Some(target) = array_compatibility::native_unpacked_shape(
                left,
                self.tree,
                self.session,
                &self.dims,
            )
        {
            array_compatibility::check_native_unpacked_assignment(
                self.tree,
                self.session,
                right,
                &target,
                || "assignment".into(),
                &self.dims,
            )?;
        }
        // A virtual interface is assigned or compared, not computed with
        // (IEEE 1800-2023 25.9).
        if op != "="
            && let Some(ty) = virtual_interfaces::native_type(
                self.tree,
                self.session,
                left,
                self.tree.declarations(),
                &self.dims,
            )
        {
            return Err(virtual_interfaces::arithmetic(op, &ty));
        }
        // An element of an array or a container of virtual interfaces is
        // given no target type below, so it is checked here (25.9).
        if op == "="
            && let Some(ty) = virtual_interfaces::native_type(
                self.tree,
                self.session,
                left,
                self.tree.declarations(),
                &self.dims,
            )
        {
            virtual_interfaces::check_assignment(
                self.tree,
                self.session,
                right,
                self.tree.declarations(),
                &self.dims,
                &ty,
            )?;
        }
        if let Some(stmt) = self.string_character_assignment(op, nonblocking, left, right)? {
            return Ok(vec![stmt]);
        }
        if let Some(stmt) = self.property_assignment(op, nonblocking, left, right)? {
            return Ok(vec![stmt]);
        }
        if let Some(stmt) = self.virtual_member_assignment(op, nonblocking, left, right)? {
            return Ok(vec![stmt]);
        }
        if let Some(stmt) = self.aggregate_assignment(op, nonblocking, left, right)? {
            return Ok(vec![stmt]);
        }
        if op != "=" {
            self.reject_tagged_integral_target(left)?;
        }
        let (mut written, _) = self.native_lvalue(left, op == "=")?;
        // An assignment operator reads its target before its value is
        // evaluated, and writes the target it read: the address of a place
        // is evaluated once, before both (IEEE 1800-2023 11.4.1).
        let mut prelude = Vec::new();
        let place = matches!(written, LValue::Object { .. });
        let saved_effects = (
            self.dims.effect_values.clone(),
            self.dims.last_indices.clone(),
        );
        if place && op != "=" {
            self.save_destination(left, true, &mut prelude)?;
            written = self.native_lvalue(left, false)?.0;
        }
        if op != "="
            && expression_binding::native_string(
                self.tree,
                self.session,
                left,
                self.tree.declarations(),
                &self.dims,
            )
        {
            return Err(unsupported(format!("operator `{op}` on a string")));
        }
        let member_context = expression_binding::member_context(
            left,
            self.tree.declarations(),
            self.tree,
            self.session,
            &self.dims,
        )?;
        let mut rhs = if op == "="
            && let Some((_, ty)) = &member_context
        {
            let mut shape = procedural::dimensions_from_type(ty);
            // An enumerated member of a union takes any value of its bits
            // (IEEE 1800-2023 6.19.3).
            if assignments::union_member(
                self.tree,
                self.session,
                left,
                self.tree.declarations(),
                &self.dims,
            )? {
                shape = shape.without_enum();
            }
            if !shape.unpacked.is_empty() {
                array_compatibility::check_native_unpacked_assignment(
                    self.tree,
                    self.session,
                    right,
                    &shape,
                    || "assignment to a structure array member".into(),
                    &self.dims,
                )?;
                if !matches!(
                    self.tree
                        .declarations()
                        .expression(expression_binding::ungroup(right, self.tree.declarations()))
                        .kind,
                    celox_sv_syntax::ExprKind::Pattern { target: None, .. }
                ) && let Some(actual) =
                    dimensions::operand_shape(right, self.tree, self.session, &self.dims)?
                    && let Some((actual, target)) = array_compatibility::incompatible(
                        self.tree,
                        self.session,
                        &actual,
                        &shape,
                        &self.dims,
                    )
                {
                    return Err(AnalyzerError::IncompatibleUnpackedArray {
                        context: "assignment to a structure array member".into(),
                        actual,
                        target,
                    });
                }
            }
            expression_binding::bind_value(
                right,
                self.tree.declarations(),
                self.tree,
                self.session,
                &self.dims,
                Some(&shape),
            )?
        } else if op == "="
            && matches!(written, LValue::Select { .. })
            && expression_binding::is_stream(right, self.tree.declarations())
            && let Some(shape) =
                dimensions::operand_shape(left, self.tree, self.session, &self.dims)?
        {
            // A stream fills the selected type, whose position may be known
            // only at run time (IEEE 1800-2023 11.4.14).
            expression_binding::bind_value(
                right,
                self.tree.declarations(),
                self.tree,
                self.session,
                &self.dims,
                Some(&shape),
            )?
        } else if op == "="
            && let Some(target) = array_compatibility::native_unpacked_shape(
                left,
                self.tree,
                self.session,
                &self.dims,
            )
            && containers::native_type(
                right,
                self.tree.declarations(),
                self.tree,
                self.session,
                &self.dims,
            )?
            .is_some()
        {
            // A fixed-size unpacked array, also a selected element of an
            // array of them, takes the elements of a dynamically sized one
            // (IEEE 1800-2023 7.6).
            expression_binding::bind_value(
                right,
                self.tree.declarations(),
                self.tree,
                self.session,
                &self.dims,
                Some(&target),
            )?
        } else if op == "="
            && let Some(value) = expression_binding::selected_pattern(
                left,
                right,
                &written,
                self.tree,
                self.session,
                &self.dims,
            )?
        {
            value
        } else if op == "=" {
            expression_binding::expression(
                right,
                self.tree.declarations(),
                &written,
                self.tree,
                self.session,
                &self.dims,
            )?
        } else {
            // `A op= B` is `A = A op B` with the address of `A` evaluated
            // once (11.4.1): the read of a place, with its checks, comes
            // before `B`, into a local of the target's type.
            let read = if place {
                let (source, unique) = self.hidden_of(left)?;
                let mut value = statements::expr_from_lvalue(self.tree, &written, &self.dims);
                self.rename_expr(&mut value);
                prelude.push(Stmt::Assign {
                    lhs: LValue::Ident(unique),
                    rhs: value,
                    nonblocking: false,
                });
                Some(LValue::Ident(source))
            } else {
                None
            };
            let value = expression_binding::value(
                right,
                self.tree.declarations(),
                self.tree,
                self.session,
                &self.dims,
            )?;
            assignment_op_expr(
                self.tree,
                self.session,
                read.as_ref().unwrap_or(&written),
                op,
                value,
                &self.dims,
            )?
        };
        if op == "="
            && expression_binding::native_string(
                self.tree,
                self.session,
                left,
                self.tree.declarations(),
                &self.dims,
            )
        {
            rhs = expression_binding::string_operand(rhs, &self.dims);
        }
        // Preserve assignment sizing before checked accesses introduce a temporary
        // (IEEE 1800-2023 11.8.2), using the selected destination width.
        // A two-state member of a four-state packed struct also drops unknown bits.
        if matches!(
            statements::variable_view(&written, &self.dims).0,
            LValue::Select {
                is_2state: true,
                ..
            }
        ) || member_context
            .as_ref()
            .is_some_and(|(guards, _)| !guards.is_empty())
        {
            rhs = coerce_procedural_assignment_rhs(self.tree, rhs, &written, &self.dims);
        }
        // A class property or container element is written through its
        // object. An assignment evaluates its value, into a local of the
        // target's type, before the target's address, which it evaluates
        // with the values it has when it is written (4.9.3); an assignment
        // operator evaluated the address already.
        if place {
            if nonblocking {
                return Err(unsupported(
                    "nonblocking assignment to a class property or container element",
                ));
            }
            let (source, unique) = self.hidden_of(left)?;
            self.rename_expr(&mut rhs);
            prelude.push(Stmt::Assign {
                lhs: LValue::Ident(unique),
                rhs,
                nonblocking: false,
            });
            if op == "=" {
                self.save_destination(left, false, &mut prelude)?;
                written = self.native_lvalue(left, true)?.0;
            }
            prelude.extend(self.emit_assign(written, Expr::Ident(source), nonblocking)?);
            (self.dims.effect_values, self.dims.last_indices) = saved_effects;
            return Ok(prelude);
        }
        let guards = member_context.map(|(guards, _)| guards).unwrap_or_default();
        if let Some(deferred) = deferred_destination_checks {
            *deferred = guards;
        } else {
            rhs = packed_structs::checked_value(guards, rhs, self.tree, &self.dims)?;
        }
        self.emit_assign(written, rhs, nonblocking)
    }

    /// The statements of `written = rhs`, both before renaming. Every
    /// assignment of a bound target is emitted here: a class property or
    /// container element, whose address the caller evaluated, is written
    /// through its object (`place_assignment`).
    pub(super) fn emit_assign(
        &mut self,
        written: LValue,
        mut rhs: Expr,
        nonblocking: bool,
    ) -> Converted<Vec<Stmt>> {
        match written {
            LValue::Object {
                place,
                select,
                guards,
                whole,
            } => self.place_assignment(*place, *select, guards, whole, rhs, nonblocking),
            mut lhs => {
                self.rename_lvalue(&mut lhs);
                self.rename_expr(&mut rhs);
                Ok(vec![Stmt::Assign {
                    lhs,
                    rhs,
                    nonblocking,
                }])
            }
        }
    }

    /// An assignment to a whole variable or member of unpacked structure
    /// type, which writes each of its leaf variables (IEEE 1800-2023 7.2).
    pub(super) fn aggregate_assignment(
        &mut self,
        op: &str,
        nonblocking: bool,
        left: celox_sv_syntax::ExprId,
        right: celox_sv_syntax::ExprId,
    ) -> Result<Option<Stmt>, AnalyzerError> {
        let Ok(written) = expression_binding::variable_lvalue(
            left,
            self.tree.declarations(),
            self.tree,
            self.session,
            &self.dims,
            op == "=",
        ) else {
            return Ok(None);
        };
        let LValue::Ident(name) = &written else {
            return Ok(None);
        };
        let Some(mut parts) = unpacked_structs::targets(name, &self.dims) else {
            return Ok(None);
        };
        if op != "=" {
            return Err(unsupported(format!(
                "operator `{op}` on an unpacked structure"
            )));
        }
        let mut rhs = expression_binding::expression(
            right,
            self.tree.declarations(),
            &written,
            self.tree,
            self.session,
            &self.dims,
        )?;
        unpacked_structs::check_assignment(name, right, self.tree, self.session, &self.dims)?;
        parts.iter_mut().for_each(|part| self.rename_lvalue(part));
        self.rename_expr(&mut rhs);
        Ok(Some(Stmt::AssignConcat {
            parts,
            rhs,
            nonblocking,
        }))
    }

    /// `str[i] = c`, which replaces one character like `str.putc(i, c)`
    /// (IEEE 1800-2023 6.16.2).
    pub(super) fn string_character_assignment(
        &mut self,
        op: &str,
        nonblocking: bool,
        left: celox_sv_syntax::ExprId,
        right: celox_sv_syntax::ExprId,
    ) -> Result<Option<Stmt>, AnalyzerError> {
        let native = self.tree.declarations();
        let celox_sv_syntax::ExprKind::Select {
            expression,
            dimension,
        } = &native
            .expression(expression_binding::ungroup(left, native))
            .kind
        else {
            return Ok(None);
        };
        if !expression_binding::native_string(
            self.tree,
            self.session,
            *expression,
            native,
            &self.dims,
        ) {
            return Ok(None);
        }
        let celox_sv_syntax::DimensionKind::Index(index) = dimension.kind else {
            return Err(unsupported("part-select of a string"));
        };
        if op != "=" {
            return Err(unsupported(format!(
                "operator `{op}` on a string character"
            )));
        }
        let index = expression_binding::value(index, native, self.tree, self.session, &self.dims)?;
        let character =
            expression_binding::value(right, native, self.tree, self.session, &self.dims)?;
        self.string_update(
            *expression,
            StringOp::Putc,
            vec![index, character],
            nonblocking,
        )
        .map(Some)
    }

    /// An assignment to a property of a class object: `h.p = v`, or `p = v`
    /// inside a method of its class (IEEE 1800-2023 8.5).
    pub(super) fn property_assignment(
        &mut self,
        op: &str,
        nonblocking: bool,
        left: celox_sv_syntax::ExprId,
        right: celox_sv_syntax::ExprId,
    ) -> Result<Option<Stmt>, AnalyzerError> {
        let native = self.tree.declarations();
        let Some((property, receiver)) =
            expression_binding::object_property(self.tree, self.session, left, native, &self.dims)?
        else {
            return Ok(None);
        };
        // A property of integral type is a place of the ordinary target
        // (`expression_binding::object_place`).
        if property.field.handle.is_none()
            && property.r#type.container.is_none()
            && property.r#type.unpacked_struct().is_none()
        {
            return Ok(None);
        }
        if nonblocking {
            return Err(unsupported("nonblocking assignment to a class property"));
        }
        let handle = expression_binding::object_receiver(
            self.tree,
            self.session,
            receiver,
            native,
            &self.dims,
        )?;
        let target = dimensions_from_type(&property.r#type);
        let bind = |session: &mut Session, right| {
            expression_binding::bind_value(
                right,
                native,
                self.tree,
                session,
                &self.dims,
                Some(&target),
            )
        };
        if let Some(container) = property.r#type.container.as_deref() {
            if op != "=" {
                return Err(unsupported("compound assignment to a container property"));
            }
            let value =
                containers::assignment(right, self.tree, self.session, &self.dims, container)?;
            return self.property_store(&property, handle, op, value).map(Some);
        }
        let mut value = match property.r#type.class() {
            Some(class) if op == "=" => {
                match expression_binding::object_constructor(
                    self.tree,
                    self.session,
                    right,
                    native,
                    &self.dims,
                    class,
                )? {
                    Some(call) => call,
                    None => {
                        expression_binding::check_handle_assignment(
                            self.tree,
                            self.session,
                            right,
                            native,
                            &self.dims,
                            class,
                            || format!("assignment to property `{}`", property.name),
                        )?;
                        bind(self.session, right)?
                    }
                }
            }
            _ if op == "=" => bind(self.session, right)?,
            _ => expression_binding::value(right, native, self.tree, self.session, &self.dims)?,
        };
        if property.field.handle == Some(system_functions::HandleProperty::String) {
            if op != "=" {
                return Err(unsupported(format!("operator `{op}` on a string")));
            }
            value = expression_binding::string_operand(value, &self.dims);
        }
        self.property_store(&property, handle, op, value).map(Some)
    }

    /// An assignment to a member of the interface instance that a virtual
    /// interface denotes, `h.m = v` or `h.m <= v` (IEEE 1800-2023 25.9).
    pub(super) fn virtual_member_assignment(
        &mut self,
        op: &str,
        nonblocking: bool,
        left: celox_sv_syntax::ExprId,
        right: celox_sv_syntax::ExprId,
    ) -> Result<Option<Stmt>, AnalyzerError> {
        let native = self.tree.declarations();
        let (handle, ty, member, selected) = match virtual_interfaces::native_member(
            self.tree,
            self.session,
            left,
            native,
            &self.dims,
        ) {
            Some((handle, ty, member)) => (handle, ty, member, None),
            None => match virtual_interfaces::native_selected_member(
                self.tree,
                self.session,
                left,
                native,
                &self.dims,
            ) {
                // The select is bound against the member's template.
                Some((handle, ty, member)) => {
                    let lowered = expression_binding::variable_lvalue(
                        left,
                        native,
                        self.tree,
                        self.session,
                        &self.dims,
                        false,
                    )?;
                    let mut renamed = lowered.clone();
                    self.rename_lvalue(&mut renamed);
                    (handle, ty, member, Some((lowered, renamed)))
                }
                None => return Ok(None),
            },
        };
        let (found, _) = virtual_interfaces::member(self.session, &ty, &member)?;
        virtual_interfaces::check_write(self.session, &ty, &member)?;
        if found.is_net {
            return Err(unsupported(format!(
                "procedural assignment to net `{member}` through a virtual interface"
            )));
        }
        let target = dimensions_from_type(&found.r#type);
        let mut value = match (&selected, op) {
            (Some((written, _)), "=") => expression_binding::expression(
                right,
                native,
                written,
                self.tree,
                self.session,
                &self.dims,
            )?,
            (None, "=") => expression_binding::bind_value(
                right,
                native,
                self.tree,
                self.session,
                &self.dims,
                Some(&target),
            )?,
            _ => expression_binding::value(right, native, self.tree, self.session, &self.dims)?,
        };
        let mut handle =
            expression_binding::value(handle, native, self.tree, self.session, &self.dims)?;
        self.rename_expr(&mut handle);
        self.rename_expr(&mut value);
        let template = virtual_interfaces::template(&ty.interface, &member);
        let stmt =
            virtual_interfaces::dispatch(self.session, handle, &ty, &member, |session, signal| {
                let lhs = match &selected {
                    Some((_, lhs @ LValue::Select { name, .. })) if *name == template => {
                        let mut lhs = lhs.clone();
                        if let LValue::Select { name, .. } = &mut lhs {
                            *name = signal;
                        }
                        lhs
                    }
                    Some(_) => {
                        return Err(unsupported(format!(
                            "assignment to a select of `{member}` through a virtual interface"
                        )));
                    }
                    None => LValue::Ident(signal),
                };
                let rhs = if op == "=" {
                    value.clone()
                } else {
                    assignment_op_expr(self.tree, session, &lhs, op, value.clone(), &self.dims)?
                };
                Ok(vec![Stmt::Assign {
                    lhs,
                    rhs,
                    nonblocking,
                }])
            })?;
        Ok(Some(stmt))
    }

    /// `h.p op value`: a store of `value` into property `property` of the
    /// object `handle`, combined with its old value by a compound `op`.
    pub(super) fn property_store(
        &self,
        property: &classes::Property,
        handle: Expr,
        op: &str,
        mut value: Expr,
    ) -> Result<Stmt, AnalyzerError> {
        expression_binding::check_writable(property, &self.dims)?;
        if op != "=" {
            let operator = match op.strip_suffix('=') {
                Some("+") => BinaryOp::Add,
                Some("-") => BinaryOp::Sub,
                Some("*") => BinaryOp::Mul,
                Some("/") => BinaryOp::Div,
                Some("%") => BinaryOp::Mod,
                Some("&") => BinaryOp::BitAnd,
                Some("|") => BinaryOp::BitOr,
                Some("^") => BinaryOp::BitXor,
                Some("<<") | Some("<<<") => BinaryOp::Shl,
                Some(">>") => BinaryOp::Shr,
                Some(">>>") => BinaryOp::Sar,
                _ => return Err(unsupported(format!("assignment operator `{op}`"))),
            };
            value = Expr::Binary {
                left: Box::new(classes::object_call(
                    system_functions::ObjectOp::Load(property.field),
                    vec![handle.clone()],
                )),
                op: operator,
                right: Box::new(value),
            };
        }
        let mut store = classes::object_call(
            system_functions::ObjectOp::Store(property.field),
            vec![handle, value],
        );
        self.rename_expr(&mut store);
        Ok(Stmt::Eval(store))
    }
}

/// Whether `expr` calls a subroutine or another function whose calls have
/// effects, such as `$urandom` (see [`Callee::is_effectful`]).
fn has_effectful_call(expr: &ConstExpr) -> bool {
    match expr {
        ConstExpr::Function { callee, args, .. } => {
            callee.is_effectful() || args.iter().any(has_effectful_call)
        }
        ConstExpr::Literal(_) | ConstExpr::Ident(_) => false,
        ConstExpr::Select { expr, bit } => has_effectful_call(expr) || has_effectful_call(bit),
        ConstExpr::SelectRange { expr, msb, lsb, .. } => {
            has_effectful_call(expr) || has_effectful_call(msb) || has_effectful_call(lsb)
        }
        ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => has_effectful_call(expr),
        ConstExpr::Binary { left, right, .. } => {
            has_effectful_call(left) || has_effectful_call(right)
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            has_effectful_call(condition)
                || has_effectful_call(then_expr)
                || has_effectful_call(else_expr)
        }
    }
}
