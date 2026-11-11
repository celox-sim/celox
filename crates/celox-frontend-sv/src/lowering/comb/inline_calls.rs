//! Inlined subroutine calls: the body is executed where the call is, with
//! its arguments and locals as hidden variables.

use super::*;

impl Comb<'_, '_> {
    /// Inline a call of `name`. Returns the function result, if any.
    pub(super) fn call(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        name: &str,
        args: &[Option<sv::ir::Expr>],
        arg_order: &[usize],
    ) -> Result<Option<Value>, sv::AnalyzerError> {
        if args.is_empty()
            && let Some(literal) = self.m.constant_functions.value(name)
        {
            let node = self.alloc(SLTNode::Constant(
                literal.value,
                literal.mask,
                literal.width,
                literal.signed,
            ))?;
            return Ok(Some((node, Sources::default())));
        }
        let subroutine = self
            .m
            .subroutine(name)
            .cloned()
            .ok_or_else(|| unsupported(format!("call of `{name}`")))?;
        if args
            .iter()
            .flatten()
            .any(|arg| !tagged_actual_guards(arg).is_empty())
        {
            return Err(unsupported("checked tagged union argument"));
        }
        // Symbolic execution cannot represent state shared by invocations.
        // Fall back to Ff, including for runtime declaration initializers.
        if !subroutine.persistent_locals.is_empty() {
            return Err(unsupported("stateful function call"));
        }
        self.m.record_call(&self.active_calls, name);
        if self.active_calls.iter().any(|active| active == name) {
            self.m
                .record_argument_calls(&self.active_calls, &subroutine, args);
            return Err(unsupported(format!("recursive call of `{name}`")));
        }
        if self.active_calls.len() >= MAX_CALL_DEPTH {
            return Err(unsupported("subroutine call depth limit exceeded"));
        }
        if args.len() > subroutine.params.len() {
            return Err(unsupported(format!(
                "function call `{name}` with too many arguments"
            )));
        }
        // Copy the inputs in, left to right, in the caller's scope.
        let mut outputs = Vec::new();
        let mut copied = Vec::new();
        let mut references = Vec::new();
        let positions: Vec<_> = if arg_order.is_empty() {
            (0..subroutine.params.len()).collect()
        } else {
            arg_order.to_vec()
        };
        for position in positions {
            let param = subroutine
                .params
                .get(position)
                .ok_or_else(|| unsupported(format!("argument position of `{name}`")))?;
            let id = self
                .m
                .id(&param.name)
                .ok_or_else(|| unsupported(format!("argument `{}`", param.source_name)))?;
            let width = self.m.var(id).width;
            let actual = subroutine
                .actual(position, args, false)
                .map_err(unsupported)?;
            if param.direction.is_reference() && !param.leaves.is_empty() {
                // Each member of a formal of unpacked structure type names
                // the matching member variable of the actual.
                let targets = match &actual {
                    Some(sv::ir::Expr::Concat(parts)) if parts.len() == param.leaves.len() => parts
                        .iter()
                        .map(|part| match part {
                            sv::ir::Expr::Ident(actual) => self.m.id(actual),
                            _ => None,
                        })
                        .collect::<Option<Vec<_>>>(),
                    _ => None,
                }
                .ok_or_else(|| {
                    unsupported(format!("ref argument `{}` of `{name}`", param.source_name))
                })?;
                for (leaf, target) in param.leaves.iter().zip(targets) {
                    let id = self
                        .m
                        .id(leaf)
                        .ok_or_else(|| unsupported(format!("argument `{}`", param.source_name)))?;
                    references.push((leaf.clone(), id, target));
                }
                continue;
            }
            if param.direction.is_reference() {
                // A reference formal names the actual variable itself
                // (IEEE 1800-2023 13.5.2); other actuals fall back to Ff.
                let target = match &actual {
                    Some(sv::ir::Expr::Ident(actual)) => self.m.id(actual),
                    _ => None,
                }
                .ok_or_else(|| {
                    unsupported(format!("ref argument `{}` of `{name}`", param.source_name))
                })?;
                references.push((param.name.clone(), id, target));
                continue;
            }
            if param.direction.is_written() {
                let actual = actual.as_ref().and_then(lvalue_from_expr).ok_or_else(|| {
                    unsupported(format!(
                        "output argument `{}` of `{name}`",
                        param.source_name
                    ))
                })?;
                outputs.push((id, actual));
            }
            if param.direction.is_read() {
                let actual = actual.ok_or_else(|| {
                    unsupported(format!(
                        "missing argument `{}` of `{name}`",
                        param.source_name
                    ))
                })?;
                let defaulted = matches!(args.get(position), None | Some(None));
                let actual = if self.m.calls(&actual) {
                    if defaulted {
                        if self.binding_defaults.iter().any(|active| active == name) {
                            return Err(unsupported(format!("recursive call of `{name}`")));
                        }
                        self.binding_defaults.push(name.to_string());
                    }
                    let hoisted = self.hoist(store, frames, &actual);
                    if defaulted {
                        self.binding_defaults.pop();
                    }
                    hoisted?
                } else {
                    actual
                };
                let signed = self.expr_signed(&actual);
                let from = floating::precision(&actual, self.m.variables, self.m.name_to_id);
                let to = self.m.var(id).float;
                let (node, sources) = self.eval(
                    store,
                    frames,
                    &actual,
                    if from.is_some() || to.is_some() {
                        None
                    } else {
                        Some((width, signed))
                    },
                )?;
                let node = floating::convert(
                    self.arena,
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
                let node =
                    coerce_node_width(self.arena, node, Some(width), signed).map_err(slt_error)?;
                let node = if self.m.var(id).is_4state {
                    node
                } else {
                    self.alloc(SLTNode::Unary(UnaryOp::ToTwoState, node))?
                };
                copied.push((id, (node, sources)));
            } else {
                let node = self.default_value(id)?;
                copied.push((id, (node, Sources::default())));
            }
        }
        for (id, value) in copied {
            let width = self.m.var(id).width;
            self.write(store, id, full(width), value)?;
        }
        for (formal, _, target) in &references {
            self.m.name_to_id.insert(formal.clone(), *target);
        }
        let return_var = match &subroutine.return_var {
            Some(name) => {
                let id = self
                    .m
                    .id(name)
                    .ok_or_else(|| unsupported(format!("result of `{}`", subroutine.name)))?;
                let node = self.default_value(id)?;
                let width = self.m.var(id).width;
                self.write(store, id, full(width), (node, Sources::default()))?;
                Some(id)
            }
            None => None,
        };
        let return_flag = self.flag("return");
        self.set_flag(store, return_flag, false)?;
        let mut call_frames = frames.to_vec();
        call_frames.push(Frame::Function {
            return_flag,
            return_var: subroutine.return_var.clone(),
        });
        let nodes = self.arena.len();
        self.active_calls.push(name.to_string());
        let result = self.exec_block(store.fork(), &call_frames, &subroutine.body);
        self.active_calls.pop();
        self.m.expanded_call(
            name,
            super::super::recursion::Expansion {
                nodes: self.arena.len() - nodes,
                ..Default::default()
            },
        );
        for (formal, original, _) in references {
            self.m.name_to_id.insert(formal, original);
        }
        *store = result?;
        // Copy the outputs back.
        for (id, lvalues) in outputs {
            let width = self.m.var(id).width;
            let signed = self.m.var(id).signed;
            let (mut node, sources) = self.read(store, id, full(width))?;
            let from = self.m.var(id).float;
            if let [target] = lvalues.as_slice() {
                let target_id = self
                    .m
                    .id(target.name())
                    .ok_or_else(|| unsupported("output argument target"))?;
                let target_var = self.m.var(target_id);
                node = floating::convert(
                    self.arena,
                    node,
                    from,
                    target_var.float,
                    target_var.width,
                    target_var.signed,
                    true,
                )
                .ok_or_else(|| unsupported("real output argument conversion"))?;
            } else if from.is_some() {
                return Err(unsupported("real output argument concatenation"));
            }
            self.write_back(store, frames, &lvalues, (node, sources), signed)?;
        }
        match return_var {
            Some(id) => {
                let width = self.m.var(id).width;
                Ok(Some(self.read(store, id, full(width))?))
            }
            None => Ok(None),
        }
    }
}
