//! Inlined subroutine calls: the body is lowered where the call is, with
//! the formals bound to the actuals.

use super::*;

impl Ff<'_, '_> {
    /// A variable of the type of property `field`, through which an
    /// assignment to part of a property formal writes (see
    /// [`references::Alias::rewrite`]). One that a unit's lowering makes is
    /// a slot of the unit's frame, so each activation has its own.
    pub(super) fn part_scratch(&mut self, field: &sv::ir::ObjectField) -> String {
        self.m
            .temp(
                "ref_part",
                field.width as usize,
                field.signed,
                field.four_state,
            )
            .1
    }

    /// The input `value` of formal `id`, or with `keep` the name of a
    /// temporary of the formal's type that holds it across a later host
    /// request.
    pub(super) fn kept_input(
        &mut self,
        id: SourceVarId,
        value: RegisterId,
        keep: bool,
    ) -> Result<RegisterId, String> {
        if !keep {
            return Ok(value);
        }
        let formal = self.m.var(id);
        let (width, signed, value_kind, float) =
            (formal.width, formal.signed, formal.value_kind, formal.float);
        let four_state = matches!(self.b.register(&value), RegisterType::Logic { .. });
        let (temp, name) = self.m.temp("input", width, signed, four_state);
        let variable = self.m.variables.get_mut(&temp).unwrap();
        variable.value_kind = value_kind;
        variable.float = float;
        self.store(temp, SIROffset::Static(0), width, value);
        Err(name)
    }

    /// Inline a call. Returns the name of the variable holding the result.
    pub(super) fn call(
        &mut self,
        name: &str,
        args: &[Option<sv::ir::Expr>],
        arg_order: &[usize],
    ) -> Result<Option<String>, sv::AnalyzerError> {
        let mut subroutine = self
            .m
            .subroutine(name)
            .cloned()
            .ok_or_else(|| unsupported(format!("call of `{name}`")))?;
        self.m.record_call(&self.active_calls, name);
        if args.len() > subroutine.params.len() {
            return Err(unsupported(format!(
                "function call `{name}` with too many arguments"
            )));
        }
        // A default that uses the object reads the receiver as the call
        // evaluated it, kept before the other arguments are evaluated
        // (IEEE 1800-2023 13.5.3).
        let receiver_kept =
            matches!(args.first(), Some(Some(_))) && subroutine.defaults_read_object(args);
        let kept_args;
        let args = match args.split_first() {
            Some((Some(receiver), rest)) if receiver_kept => {
                let this = self
                    .m
                    .id(&subroutine.params[0].name)
                    .ok_or_else(|| unsupported(format!("object of `{name}`")))?;
                let value = self.eval(receiver, None)?;
                let Err(kept) = self.kept_input(this, value, true) else {
                    unreachable!("a kept input is kept")
                };
                kept_args = std::iter::once(Some(sv::ir::Expr::Ident(kept)))
                    .chain(rest.iter().cloned())
                    .collect::<Vec<_>>();
                &kept_args[..]
            }
            _ => args,
        };
        // Evaluate every input before any formal is written: an argument may
        // read a formal of an enclosing call of another subroutine.
        let mut outputs = Vec::new();
        let mut inputs = Vec::new();
        let mut references = Vec::new();
        let mut aliases = Vec::new();
        // The name of each `ref` formal's actual that is a variable.
        let mut actual_names: HashMap<String, String> = HashMap::default();
        let mut property_outputs = Vec::new();
        let mut place_outputs = Vec::new();
        // The formals an out-of-line call copies back, each with the
        // temporary that receives it from the callee's frame.
        let mut copies = Vec::new();
        let positions: Vec<_> = if arg_order.is_empty() {
            (0..subroutine.params.len()).collect()
        } else {
            arg_order.to_vec()
        };
        // The objects and positions that references capture come first (the
        // order of evaluation of arguments is undefined, IEEE 1800-2023
        // 13.5): keeping one in a forked activation's frame starts a new
        // resume point, which no evaluated input may span.
        for &position in &positions {
            let Some(param) = subroutine
                .params
                .get(position)
                .filter(|param| param.direction.is_reference())
            else {
                continue;
            };
            let id = self
                .m
                .id(&param.name)
                .ok_or_else(|| unsupported(format!("argument `{}`", param.source_name)))?;
            let actual = subroutine
                .actual(position, args, receiver_kept)
                .map_err(unsupported)?;
            if param.leaves.is_empty() {
                if let Some(sv::ir::Expr::Ident(actual)) = &actual {
                    actual_names.insert(param.name.clone(), actual.clone());
                }
                match self.reference(name, param, actual)? {
                    Reference::Variable(target) => {
                        references.push((param.name.clone(), id, target));
                    }
                    Reference::Alias(alias) => aliases.push(*alias),
                }
                continue;
            }
            // Each member of a formal of unpacked structure type refers to
            // the matching member of the actual, which the analyzer binds as
            // the concatenation of its members.
            let parts = match actual {
                Some(sv::ir::Expr::Concat(parts)) if parts.len() == param.leaves.len() => parts,
                _ => {
                    return Err(unsupported(format!(
                        "ref argument `{}` of `{name}`",
                        param.source_name
                    )));
                }
            };
            for (leaf, part) in param.leaves.iter().zip(parts) {
                let id = self
                    .m
                    .id(leaf)
                    .ok_or_else(|| unsupported(format!("argument `{}`", param.source_name)))?;
                let member = sv::ir::SubroutineParam {
                    name: leaf.clone(),
                    leaves: Vec::new(),
                    default: None,
                    ..param.clone()
                };
                match self.reference(name, &member, Some(part))? {
                    Reference::Variable(target) => references.push((leaf.clone(), id, target)),
                    Reference::Alias(alias) => aliases.push(*alias),
                }
            }
        }
        // An out-of-line call passes what each `ref` formal refers to as a
        // place (see `out_of_line::Place`), whose static part selects the
        // function.
        let places = self.ref_places(&subroutine, &references, &aliases, &actual_names);
        let out_of_line = self.out_of_line_callee(name, &subroutine, args, places.as_deref())?;
        if out_of_line.is_none() && self.active_calls.iter().any(|active| active == name) {
            self.m
                .record_argument_calls(&self.active_calls, &subroutine, args);
            return Err(unsupported(format!("recursive call of `{name}`")));
        }
        if out_of_line.is_none() && self.active_calls.len() >= MAX_CALL_DEPTH {
            return Err(unsupported("subroutine call depth limit exceeded"));
        }
        // Each inlined automatic task activation owns persistent storage, so
        // another process can enter the same task while this one is suspended.
        // Static formals/locals deliberately keep their module-wide addresses.
        // An out-of-line call keeps its automatic storage in its frame.
        let mut activation = Vec::new();
        let copies_before = self.m.activation_copies.len();
        if out_of_line.is_none() && self.kernel.is_some() && (subroutine.is_task || self.forked) {
            for name in &subroutine.automatic_locals {
                // A reference formal, or a member of one, has no storage of
                // its own.
                if subroutine.params.iter().any(|param| {
                    param.direction.is_reference()
                        && (&param.name == name || param.leaves.contains(name))
                }) {
                    continue;
                }
                let original = self
                    .m
                    .id(name)
                    .ok_or_else(|| unsupported(format!("task local `{name}`")))?;
                let variable = self.m.var(original).clone();
                let (copy, copy_name) = self.m.temp(
                    "task_local",
                    variable.width,
                    variable.signed,
                    variable.is_4state,
                );
                self.m.variables.insert(
                    copy,
                    SvVariable {
                        path: vec![copy_name],
                        activation_scratch: true,
                        ..variable
                    },
                );
                if let Some(width) = self.element_widths.get(&stable(original)).copied() {
                    self.element_widths.insert(stable(copy), width);
                    self.element_widths.insert(working(copy), width);
                }
                self.m.name_to_id.insert(name.clone(), copy);
                self.m
                    .activation_copies
                    .push((name.clone(), original, copy));
                if self.forked {
                    let slot = self.frame_locals.len();
                    self.frame_locals.push(copy);
                    self.with_kernel(|kernel| kernel.local(slot))
                        .map_err(kernel_error)?;
                }
                activation.push((name.clone(), original));
            }
        }
        // An input evaluates into a register, which a host request in a later
        // input, a default included, would lose: each input before the last
        // one that can make a request is kept, converted to its formal, in a
        // temporary until the formals are written.
        let last_request = positions.iter().rposition(|&position| {
            subroutine.params.get(position).is_some_and(|param| {
                param.direction.is_read()
                    && !param.direction.is_reference()
                    && subroutine
                        .actual(position, args, receiver_kept)
                        .ok()
                        .flatten()
                        .is_some_and(|actual| self.m.makes_host_request(&actual))
            })
        });
        // An out-of-line call passes the arguments it omits for the unit to
        // evaluate their defaults (see `out_of_line::Function::omitted`).
        let omitted = out_of_line.and_then(|index| {
            self.m
                .out_of_line()
                .and_then(|functions| functions.functions[index].omitted.clone())
        });
        let mut omitted_bits = 0u64;
        for (index, position) in positions.into_iter().enumerate() {
            let keep = last_request.is_some_and(|last| index < last);
            let param = subroutine
                .params
                .get(position)
                .ok_or_else(|| unsupported(format!("argument position of `{name}`")))?;
            if param.direction.is_reference() {
                continue;
            }
            if let Some((_, defaulted)) = &omitted
                && defaulted.contains(&position)
                && matches!(args.get(position), None | Some(None))
            {
                omitted_bits |= 1 << position;
                continue;
            }
            let id = self
                .m
                .id(&param.name)
                .ok_or_else(|| unsupported(format!("argument `{}`", param.source_name)))?;
            let actual = subroutine
                .actual(position, args, receiver_kept)
                .map_err(unsupported)?;
            if param.direction.is_written() {
                let reference = actual
                    .as_ref()
                    .ok_or_else(|| unsupported("missing output argument"))?;
                // An out-of-line call copies a formal of its frame back from
                // a temporary of the caller.
                let declared = self.m.declared(id);
                let formal = match out_of_line {
                    Some(index)
                        if self.m.out_of_line().is_some_and(|functions| {
                            functions.functions[index].slot(declared).is_some()
                        }) =>
                    {
                        let (temp, name) = self.call_result_temp(id);
                        copies.push((id, temp));
                        name
                    }
                    _ => param.name.clone(),
                };
                // A place actual, a class property, a select or member of
                // one or a container element, is assigned when the call
                // returns: "Execution of the return from the subroutine shall
                // pass values from the output and inout type arguments to
                // the corresponding variables in the subroutine call" (IEEE
                // 1800-2023 13.5). The place is found then, as the
                // assignment `actual = formal` there would find it.
                if place_actual(reference) {
                    place_outputs.push((formal.clone(), reference.clone()));
                } else if let Some(store) =
                    forwarded_property(reference, sv::ir::Expr::Ident(formal.clone()))
                {
                    property_outputs.push(store);
                } else if let Some(stores) = forwarded_members(reference, &formal) {
                    property_outputs.extend(stores);
                } else {
                    let lvalues = lvalue_from_expr(reference).ok_or_else(|| {
                        unsupported(format!(
                            "output argument `{}` of `{name}`",
                            param.source_name
                        ))
                    })?;
                    let guards = tagged_actual_guards(reference);
                    self.check_tagged_guards(&guards)?;
                    // The members of an unpacked structure take the formal's
                    // bits, even a single string or handle member.
                    let concat = matches!(reference, sv::ir::Expr::Concat(_));
                    outputs.push((formal, id, lvalues, guards, concat));
                }
            }
            if param.direction.is_read() {
                let actual = actual.ok_or_else(|| {
                    unsupported(format!(
                        "missing argument `{}` of `{name}`",
                        param.source_name
                    ))
                })?;
                let actual = self.string_conversion(id, actual);
                let defaulted = matches!(args.get(position), None | Some(None));
                let actual = if self.m.calls(&actual) {
                    if defaulted {
                        if self.binding_defaults.iter().any(|active| active == name) {
                            return Err(unsupported(format!("recursive call of `{name}`")));
                        }
                        self.binding_defaults.push(name.to_string());
                    }
                    let hoisted = self.hoist(&actual);
                    if defaulted {
                        self.binding_defaults.pop();
                    }
                    hoisted?
                } else {
                    actual
                };
                let width = self.m.var(id).width;
                let signed = self.expr_signed(&actual);
                let from = floating::precision(&actual, self.m.variables, self.m.name_to_id);
                let to = self.m.var(id).float;
                let mut arena = SLTNodeArena::new();
                let node = self.expr_slt(
                    &actual,
                    if from.is_some() || to.is_some() {
                        None
                    } else {
                        Some((width, signed))
                    },
                    &mut arena,
                )?;
                let node = floating::convert(
                    &mut arena,
                    node,
                    from,
                    to,
                    width,
                    if from.is_none() {
                        signed
                    } else {
                        self.m.var(id).signed
                    },
                    true,
                )
                .ok_or_else(|| unsupported("real argument conversion"))?;
                let mut node =
                    coerce_node_width(&mut arena, node, Some(width), signed).map_err(slt_error)?;
                if !self.m.var(id).is_4state {
                    node = arena
                        .alloc(SLTNode::Unary(UnaryOp::ToTwoState, node))
                        .map_err(slt_error)?;
                }
                let value = self.lower_slt(&arena, node)?;
                inputs.push((id, self.kept_input(id, value, keep)));
            } else if !subroutine.persistent_locals.contains(&param.name)
                && (out_of_line.is_some()
                    || self.kernel.is_none()
                    || !subroutine.is_task
                    || subroutine.automatic_locals.contains(&param.name))
            {
                let value = self.default_reg(id);
                inputs.push((id, self.kept_input(id, value, keep)));
            }
        }
        if let (Some(index), Some((slot, _))) = (out_of_line, &omitted) {
            let functions = self.m.out_of_line().expect("calling out of line");
            let id = functions.functions[index].slots[*slot].id;
            let bits = self.b.alloc_bit(64, false);
            self.b
                .emit(SIRInstruction::Imm(bits, SIRValue::new(omitted_bits)));
            inputs.push((id, Ok(bits)));
        }
        // The dynamic parts of the places pass through the frame: the
        // handle of a property, the positions of an element, and the value
        // of an automatic of the calling unit, which also comes back.
        if let (Some(index), Some(places)) = (out_of_line, &places) {
            let functions = self.m.out_of_line().expect("calling out of line");
            let function = &functions.functions[index];
            let slots: Vec<Vec<SourceVarId>> = function
                .places
                .iter()
                .map(|own| own.iter().map(|&slot| function.slots[slot].id).collect())
                .collect();
            for (place, ids) in places.iter().zip(slots) {
                use super::super::out_of_line::Place;
                match place {
                    Place::Variable(_) => {}
                    Place::Property { handle, .. } => inputs.push((ids[0], Err(handle.clone()))),
                    Place::Element { positions, .. } => {
                        for (&id, position) in ids.iter().zip(positions) {
                            inputs.push((id, Err(position.clone())));
                        }
                    }
                    Place::Frame { id, name } => {
                        inputs.push((ids[0], Err(name.clone())));
                        copies.push((ids[0], *id));
                    }
                }
            }
        }
        let called = match out_of_line {
            Some(index) => Some(self.call_out_of_line(index, &subroutine, inputs, &copies)?),
            None => {
                for (id, value) in inputs {
                    let value = match value {
                        Ok(value) => value,
                        Err(name) => self.eval(&sv::ir::Expr::Ident(name), None)?,
                    };
                    let width = self.m.var(id).width;
                    let offset = sv_memory_offset(self.m.var(id), 0, width);
                    self.store(id, offset, width, value);
                }
                // The body names each reference formal's actual itself, so its reads
                // and writes are those of the actual, also across suspensions of a
                // task and through any other name of it (IEEE 1800-2023 13.5.2).
                for (formal, _, target) in &references {
                    self.m.name_to_id.insert(formal.clone(), *target);
                }
                if let Some(return_var) = &subroutine.return_var {
                    let id = self
                        .m
                        .id(return_var)
                        .ok_or_else(|| unsupported(format!("result of `{name}`")))?;
                    if !subroutine.persistent_locals.contains(return_var) {
                        self.init_default(id);
                    }
                }
                let return_block = self.b.new_block();
                self.functions.push(FunctionBlocks {
                    return_block,
                    return_var: subroutine.return_var.clone(),
                    loop_depth: self.loops.len(),
                    block_depth: self.blocks.len(),
                });
                let mut body = std::mem::take(&mut subroutine.body);
                let (constants, parameter_types) = (self.m.constants, self.m.parameter_types);
                let constant = |position: &sv::ir::ConstExpr| {
                    sv::typecheck::eval_const_expr_with_types(position, constants, parameter_types)
                };
                self.split_property_concats(&mut body, &aliases);
                for alias in &aliases {
                    body = alias.rewrite(body, &constant, &mut |field| self.part_scratch(field))?;
                }
                // A fork of a body inlined into a unit shares variables as
                // the unit's own body does.
                let (body, shared) = self.share_forked(&subroutine, body)?;
                let built = self.b.built();
                self.active_calls.push(name.to_string());
                let falls = self.exec_block(&body);
                self.shared_boxes.truncate(self.shared_boxes.len() - shared);
                self.active_calls.pop();
                let after = self.b.built();
                self.m.expanded_call(
                    name,
                    super::super::recursion::Expansion {
                        instructions: after.instructions - built.instructions,
                        blocks: after.blocks - built.blocks,
                        nodes: 0,
                    },
                );
                self.functions.pop();
                for (formal, original, _) in references {
                    self.m.name_to_id.insert(formal, original);
                }
                if falls? {
                    self.jump(return_block);
                }
                self.b.switch_to_block(return_block);
                None
            }
        };
        for (formal, id, lvalues, guards, concat) in outputs {
            self.check_tagged_guards(&guards)?;
            let signed = self.m.var(id).signed;
            if let [lvalue] = lvalues.as_slice()
                && !concat
            {
                self.assign(lvalue, &sv::ir::Expr::Ident(formal))?;
            } else {
                let _ = signed;
                self.assign_concat(&lvalues, &sv::ir::Expr::Ident(formal))?;
            }
            self.observe_assignment(&lvalues)?;
        }
        for (formal, actual) in place_outputs {
            let (captured, _, _) = self.capture_system_actual(&actual)?;
            self.write_system_actual(&captured, &sv::ir::Expr::Ident(formal))?;
        }
        for store in property_outputs {
            self.exec_block(&[store])?;
        }
        let result = match called {
            Some(result) => result,
            None => subroutine.return_var.as_ref().map(|name| {
                if activation.iter().any(|(local, _)| local == name) {
                    self.m.var(self.m.id(name).unwrap()).path[0].clone()
                } else {
                    name.clone()
                }
            }),
        };
        for (name, original) in activation {
            self.m.name_to_id.insert(name, original);
        }
        self.m.activation_copies.truncate(copies_before);
        Ok(result)
    }

    /// Rewrite each assignment in `body` to a concatenation that has members
    /// of a structure reference formal bound to a class property, which
    /// [`references::Alias::rewrite`] stores one property at a time: the
    /// concatenation writes temporaries in their place, so every position
    /// of its other parts is taken before any write, and each property is
    /// then stored from its temporary.
    pub(super) fn split_property_concats(
        &mut self,
        body: &mut [sv::ir::Stmt],
        aliases: &[references::Alias],
    ) {
        let properties: HashSet<&str> = aliases
            .iter()
            .filter(|alias| matches!(alias.target, references::Target::Property { .. }))
            .map(|alias| alias.formal.as_str())
            .collect();
        if properties.is_empty() {
            return;
        }
        for stmt in body {
            stmt.walk_mut(&mut |stmt| {
                let sv::ir::Stmt::AssignConcat {
                    parts,
                    nonblocking,
                    ..
                } = stmt
                else {
                    return;
                };
                if !parts.iter().any(|part| properties.contains(part.name())) {
                    return;
                }
                let nonblocking = *nonblocking;
                let mut stores = Vec::new();
                for part in parts.iter_mut() {
                    if !matches!(part, sv::ir::LValue::Ident(name) if properties.contains(name.as_str()))
                    {
                        continue;
                    }
                    // A select of a member is left to the rewrite to reject.
                    let Ok(width) = self.lvalue_width(part) else {
                        continue;
                    };
                    let (_, temp) = self.m.temp("ref_member", width, false, self.m.four_state);
                    let member = std::mem::replace(part, sv::ir::LValue::Ident(temp.clone()));
                    stores.push(sv::ir::Stmt::Assign {
                        lhs: member,
                        rhs: sv::ir::Expr::Ident(temp),
                        nonblocking,
                    });
                }
                if stores.is_empty() {
                    return;
                }
                let assignment = std::mem::replace(stmt, sv::ir::Stmt::Break);
                stores.insert(0, assignment);
                *stmt = sv::ir::Stmt::If {
                    condition: sv::ir::Expr::Literal("1'b1".into()),
                    then_body: stores,
                    else_body: Vec::new(),
                };
            });
        }
    }

    /// What reference formal `param` of `name` refers to for a call with
    /// `actual`. The object of a property is evaluated here, at the call.
    pub(super) fn reference(
        &mut self,
        name: &str,
        param: &sv::ir::SubroutineParam,
        actual: Option<sv::ir::Expr>,
    ) -> Result<Reference, sv::AnalyzerError> {
        match actual {
            Some(sv::ir::Expr::Ident(actual)) => self
                .m
                .id(&actual)
                .map(Reference::Variable)
                .ok_or_else(|| unsupported(format!("reference to `{actual}`"))),
            Some(sv::ir::Expr::Call {
                callee: sv::ir::Callee::Object(op),
                args,
                ..
            }) if matches!(*op, sv::ir::ObjectOp::Load(_)) => {
                let sv::ir::ObjectOp::Load(field) = *op else {
                    unreachable!()
                };
                let [Some(object)] = args.as_slice() else {
                    return Err(unsupported("class property receiver"));
                };
                let (handle, handle_name) = self.m.temp(references::OBJECT_TEMP, 32, false, false);
                self.m.variables.get_mut(&handle).unwrap().value_kind =
                    celox_design::ValueKind::ObjRef;
                // A forked activation keeps its own object.
                if self.forked {
                    let slot = self.frame_locals.len();
                    self.frame_locals.push(handle);
                    self.with_kernel(|kernel| kernel.local(slot))
                        .map_err(kernel_error)?;
                }
                let object = self.handle(object)?;
                self.store(handle, SIROffset::Static(0), 32, object);
                Ok(Reference::Alias(Box::new(references::Alias {
                    formal: param.name.clone(),
                    source_name: param.source_name.clone(),
                    target: references::Target::Property {
                        handle: handle_name,
                        field,
                    },
                })))
            }
            // An element of a fixed-size unpacked array, which the analyzer
            // has checked: its index is evaluated at the call.
            Some(actual @ sv::ir::Expr::Select { .. }) => {
                let signed = self.expr_signed(&actual);
                let element = match lvalue_from_expr(&actual).as_deref() {
                    Some([element @ sv::ir::LValue::Select { .. }]) => element.clone(),
                    _ => {
                        return Err(unsupported(format!(
                            "ref argument `{}` of `{name}`",
                            param.source_name
                        )));
                    }
                };
                let element = self.hoist_lvalue(&element)?;
                // A forked activation keeps its own copies of the positions.
                self.frame_positions = true;
                let element = self.freeze_lvalue(element);
                self.frame_positions = false;
                let element = element?;
                let width = self.m.var(self.m.id(&param.name).expect("a formal")).width;
                let total = self
                    .m
                    .var(self.m.id(element.name()).expect("an array"))
                    .width;
                Ok(Reference::Alias(Box::new(references::Alias {
                    formal: param.name.clone(),
                    source_name: param.source_name.clone(),
                    target: references::Target::Element {
                        element: Box::new(element),
                        signed,
                        width,
                        total,
                    },
                })))
            }
            Some(_) => Err(unsupported(format!(
                "ref argument `{}` of `{name}` bound to a structure member",
                param.source_name
            ))),
            None => Err(unsupported(format!(
                "missing argument `{}` of `{name}`",
                param.source_name
            ))),
        }
    }
}

/// What a reference formal names during an inlined call.
pub(super) enum Reference {
    Variable(SourceVarId),
    Alias(Box<references::Alias>),
}

/// Whether output actual `actual` is a place that the analyzer bound: a
/// class property, a select of one, or a container element, under the
/// checks of the tags its selection crosses (see `capture_place_actual`).
/// A property of the object of a reference formal is forwarded instead.
pub(super) fn place_actual(actual: &sv::ir::Expr) -> bool {
    use sv::ir::{Callee, ContainerOp, Expr, ObjectOp};
    let mut place = actual;
    while let Expr::Call {
        callee: Callee::Checked { .. },
        args,
        ..
    } = place
        && let [Some(_), Some(value)] = args.as_slice()
    {
        place = value;
    }
    if let Expr::Select { expr, .. } = place {
        place = expr;
    }
    match place {
        Expr::Call {
            callee: Callee::Object(op),
            args,
            ..
        } => match (&**op, args.as_slice()) {
            (ObjectOp::Load(_), [Some(Expr::Ident(handle))])
                if handle.starts_with(references::OBJECT_TEMP) =>
            {
                false
            }
            (ObjectOp::Load(_), [Some(_)]) => true,
            (
                ObjectOp::Container {
                    op: ContainerOp::Get,
                    ..
                },
                [Some(_), Some(_)],
            ) => true,
            _ => false,
        },
        _ => false,
    }
}

pub(super) fn forwarded_property(
    actual: &sv::ir::Expr,
    value: sv::ir::Expr,
) -> Option<sv::ir::Stmt> {
    let sv::ir::Expr::Call {
        callee: sv::ir::Callee::Object(op),
        args,
        ..
    } = actual
    else {
        return None;
    };
    let (sv::ir::ObjectOp::Load(field), [Some(sv::ir::Expr::Ident(handle))]) =
        (&**op, args.as_slice())
    else {
        return None;
    };
    handle.starts_with(references::OBJECT_TEMP).then(|| {
        sv::ir::Stmt::Eval(sv::ir::Expr::Call {
            callee: sv::ir::Callee::Object(Box::new(sv::ir::ObjectOp::Store(*field))),
            args: vec![Some(sv::ir::Expr::Ident(handle.clone())), Some(value)],
            arg_order: Vec::new(),
        })
    })
}

/// The stores that copy the members of formal `formal` of unpacked
/// structure type back into the class properties that `actual`, the members
/// of a structure reference formal bound to a property, read.
pub(super) fn forwarded_members(actual: &sv::ir::Expr, formal: &str) -> Option<Vec<sv::ir::Stmt>> {
    let sv::ir::Expr::Concat(parts) = actual else {
        return None;
    };
    let widths = parts
        .iter()
        .map(|part| match part {
            sv::ir::Expr::Call {
                callee: sv::ir::Callee::Object(op),
                ..
            } => match **op {
                sv::ir::ObjectOp::Load(field) => Some(field.width as usize),
                _ => None,
            },
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let mut lsb = widths.iter().sum::<usize>();
    parts
        .iter()
        .zip(widths)
        .map(|(part, width)| {
            lsb -= width;
            let bits = sv::ir::Expr::Select {
                expr: Box::new(sv::ir::Expr::Ident(formal.to_owned())),
                msb: sv::ir::ConstExpr::Literal((lsb + width - 1).to_string()),
                lsb: sv::ir::ConstExpr::Literal(lsb.to_string()),
                signed: false,
            };
            forwarded_property(part, bits)
        })
        .collect()
}
