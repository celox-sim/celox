//! Output and inout actuals of system functions and host requests: an actual
//! is captured before the call and written after it.

use super::*;

impl Ff<'_, '_> {
    pub(super) fn check_tagged_guards(
        &mut self,
        guards: &[sv::ir::Expr],
    ) -> Result<(), sv::AnalyzerError> {
        for guard in guards {
            self.system_task(
                sv::ir::SystemTask::Assert,
                &[
                    sv::ir::SystemTaskArg::Expr(guard.clone()),
                    sv::ir::SystemTaskArg::Str(
                        "tagged union member does not match the active tag".into(),
                    ),
                ],
                None,
                None,
            )?;
        }
        Ok(())
    }

    /// Capture an inout/output actual's reference before reading or writing
    /// it, including selectors changed by the call or by other selectors.
    /// A class property or a container element, or a select of one, is
    /// captured as its place: the object or container and the index are
    /// evaluated now, and the value is written through a local of its type
    /// (see [`SystemActual`]).
    pub(super) fn capture_system_actual(
        &mut self,
        expr: &sv::ir::Expr,
    ) -> Result<(SystemActual, sv::ir::Expr, Vec<sv::ir::Expr>), sv::AnalyzerError> {
        use sv::ir::{Callee, Expr, LValue};
        // A place comes with the checks of the tags its selection crosses,
        // which its write applies as a variable's does.
        let mut place = expr;
        while let Expr::Call {
            callee: Callee::Checked { .. },
            args,
            ..
        } = place
            && let [Some(_), Some(value)] = args.as_slice()
        {
            place = value;
        }
        if let Some((actual, read)) =
            self.capture_place_actual(place, &tagged_actual_guards(expr))?
        {
            return Ok((actual, read, Vec::new()));
        }
        let parts =
            lvalue_from_expr(expr).ok_or_else(|| unsupported("integral output/inout actual"))?;
        let guards = tagged_actual_guards(expr);
        self.check_tagged_guards(&guards)?;
        let signed = self.expr_signed(expr);
        let mut captured = Vec::new();
        for part in parts {
            let part = self.hoist_lvalue(&part)?;
            captured.push(self.freeze_lvalue(part)?);
        }
        let reads = captured
            .iter()
            .map(|part| match part {
                LValue::Ident(name) => Expr::Ident(name.clone()),
                LValue::Select { name, msb, lsb, .. } => Expr::Select {
                    expr: Box::new(Expr::Ident(name.clone())),
                    msb: msb.clone(),
                    lsb: lsb.clone(),
                    signed,
                },
            })
            .collect::<Vec<_>>();
        let read = if reads.len() == 1 {
            reads.into_iter().next().unwrap()
        } else {
            Expr::Concat(reads)
        };
        Ok((
            SystemActual {
                parts: captured,
                place: None,
            },
            read,
            guards,
        ))
    }

    /// A temporary that holds a value from before a host request to after
    /// it, kept by a forked activation of its own.
    pub(super) fn request_temp(
        &mut self,
        purpose: &str,
        width: usize,
        signed: bool,
        four_state: bool,
        kind: celox_design::ValueKind,
    ) -> Result<(SourceVarId, String), sv::AnalyzerError> {
        let (id, name) = self.m.temp(purpose, width, signed, four_state);
        self.m
            .variables
            .get_mut(&id)
            .expect("a temporary")
            .value_kind = kind;
        if self.activation.forked {
            let slot = self.activation.frame_locals.len();
            self.activation.frame_locals.push(id);
            self.with_kernel(|kernel| kernel.local(slot))
                .map_err(kernel_error)?;
        }
        Ok((id, name))
    }

    /// [`Self::capture_system_actual`] of a class property of integral type,
    /// `h.p`, or an element of a container, `q[i]`, or a select of one: its
    /// object, container and index are evaluated now, in that order, into
    /// temporaries, and its select positions frozen as a variable's are.
    pub(super) fn capture_place_actual(
        &mut self,
        expr: &sv::ir::Expr,
        guards: &[sv::ir::Expr],
    ) -> Result<Option<(SystemActual, sv::ir::Expr)>, sv::AnalyzerError> {
        use sv::ir::{Callee, ContainerOp, Expr, LValue, ObjectOp};
        let (place, select) = match expr {
            Expr::Select {
                expr: place,
                msb,
                lsb,
                ..
            } => (&**place, Some((msb, lsb))),
            place => (place, None),
        };
        let Expr::Call {
            callee: Callee::Object(op),
            args,
            ..
        } = place
        else {
            return Ok(None);
        };
        let (store, args, width, four_state) = match (&**op, args.as_slice()) {
            // A property holding a handle is written whole.
            (ObjectOp::Load(field), [Some(handle)])
                if field.handle.is_none() || select.is_none() =>
            {
                let object = self.capture_handle(handle)?;
                (
                    ObjectOp::Store(*field),
                    vec![object],
                    field.width as usize,
                    field.four_state && self.m.four_state,
                )
            }
            (
                ObjectOp::Container {
                    op: ContainerOp::Get,
                    layout,
                },
                [Some(container), Some(index)],
            ) if layout.handle.is_none() => {
                // A container variable names its elements; the container
                // of a property is the one of the property's object.
                let container = match container {
                    Expr::Ident(_) => container.clone(),
                    Expr::Call {
                        callee: Callee::Object(load),
                        args,
                        arg_order,
                    } if matches!(**load, ObjectOp::Load(_)) => {
                        let [Some(handle)] = args.as_slice() else {
                            return Err(unsupported("container property receiver"));
                        };
                        Expr::Call {
                            callee: Callee::Object(load.clone()),
                            args: vec![Some(self.capture_handle(handle)?)],
                            arg_order: arg_order.clone(),
                        }
                    }
                    _ => return Err(unsupported("output actual of a selected container")),
                };
                let index = self.hoist(index)?;
                let index_width = self
                    .m
                    .comparison_width(&index, &index)
                    .ok_or_else(|| unsupported("container index output actual"))?;
                let index_signed = self.expr_signed(&index);
                let (id, name) = self.request_temp(
                    "actual_index",
                    index_width,
                    index_signed,
                    self.m.four_state,
                    celox_design::ValueKind::Bits,
                )?;
                let value = self.eval(&index, Some((index_width, index_signed)))?;
                self.store(id, SIROffset::Static(0), index_width, value);
                (
                    ObjectOp::Container {
                        op: ContainerOp::Set,
                        layout: *layout,
                    },
                    vec![container, Expr::Ident(name)],
                    layout.width as usize,
                    !layout.two_state && self.m.four_state,
                )
            }
            _ => return Ok(None),
        };
        let load = Expr::Call {
            callee: Callee::Object(op.clone()),
            args: args.iter().cloned().map(Some).collect(),
            arg_order: Vec::new(),
        };
        // The local is written after the request, from its result.
        let (id, local) = self.m.temp("actual_value", width, false, four_state);
        let float = self.m.expr_float_precision(&load);
        let variable = self.m.variables.get_mut(&id).expect("a temporary");
        variable.float = float;
        // The local of a property holding a handle keeps its object.
        if let ObjectOp::Store(field) = &store
            && let Some(handle) = field.handle
        {
            variable.value_kind = match handle {
                sv::ir::HandleProperty::Object => celox_design::ValueKind::ObjRef,
                sv::ir::HandleProperty::String => celox_design::ValueKind::StrRef,
            };
        }
        let part = match select {
            Some((msb, lsb)) => {
                let part = LValue::Select {
                    name: local.clone(),
                    msb: msb.clone(),
                    lsb: lsb.clone(),
                    array_slice_width: None,
                    array_slice_reversed: false,
                    window: None,
                };
                let part = self.hoist_lvalue(&part)?;
                self.freeze_lvalue(part)?
            }
            None => LValue::Ident(local.clone()),
        };
        let read = match &part {
            LValue::Select { msb, lsb, .. } => Expr::Select {
                expr: Box::new(load.clone()),
                msb: msb.clone(),
                lsb: lsb.clone(),
                signed: self.expr_signed(expr),
            },
            LValue::Ident(_) => load.clone(),
        };
        let store = Expr::Call {
            callee: Callee::Object(Box::new(store)),
            args: args
                .into_iter()
                .chain([Expr::Ident(local.clone())])
                .map(Some)
                .collect(),
            arg_order: Vec::new(),
        };
        // The place's checks read the value at the place it captured, which
        // its local holds when it is written.
        let guards = guards
            .iter()
            .map(|guard| {
                guard
                    .substitute(sv::ir::PLACE_VALUE, &Expr::Ident(local.clone()))
                    .ok_or_else(|| unsupported("check of an output place"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Some((
            SystemActual {
                parts: vec![part],
                place: Some(SystemPlace {
                    local,
                    load,
                    store,
                    guards,
                }),
            },
            read,
        )))
    }

    /// A class handle evaluated now into a temporary that keeps its object.
    pub(super) fn capture_handle(
        &mut self,
        handle: &sv::ir::Expr,
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let (id, name) = self.request_temp(
            "actual_object",
            32,
            false,
            false,
            celox_design::ValueKind::ObjRef,
        )?;
        let object = self.handle(handle)?;
        self.store(id, SIROffset::Static(0), 32, object);
        Ok(sv::ir::Expr::Ident(name))
    }

    /// Write `value` to output actual `actual`, as an assignment to it: a
    /// place takes its local, which a select of it writes into after the
    /// local reads the place's value.
    pub(super) fn write_system_actual(
        &mut self,
        actual: &SystemActual,
        value: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        if let Some(place) = &actual.place
            && matches!(actual.parts.as_slice(), [sv::ir::LValue::Select { .. }])
        {
            self.assign(&sv::ir::LValue::Ident(place.local.clone()), &place.load)?;
            self.check_tagged_guards(&place.guards)?;
        }
        match actual.parts.as_slice() {
            [one] => self.assign(one, value)?,
            parts => self.assign_concat(parts, value)?,
        }
        if let Some(place) = &actual.place {
            self.store_place(&place.store)?;
        }
        Ok(())
    }

    pub(super) fn host_hidden(
        &mut self,
        purpose: &str,
        expr: &sv::ir::Expr,
        width: usize,
        signed: bool,
        four_state: bool,
    ) -> Result<(SourceVarId, sv::ir::Expr), sv::AnalyzerError> {
        let value = self.eval(expr, Some((width, signed)))?;
        let (id, name) = self.m.temp(purpose, width, signed, four_state);
        self.store(id, SIROffset::Static(0), width, value);
        Ok((id, sv::ir::Expr::Ident(name)))
    }
}

/// An output or inout actual of a system function, captured by
/// [`Ff::capture_system_actual`]: the variable parts it writes, or, for a
/// class property or container element, the part of the local that holds
/// its value, which the place then takes.
pub(in crate::lowering) struct SystemActual {
    pub(in crate::lowering) parts: Vec<sv::ir::LValue>,
    pub(super) place: Option<SystemPlace>,
}

/// The local of a place an output actual writes, its read of the place,
/// its store into it, and the checks of the tags its selection crosses,
/// which read the local.
pub(super) struct SystemPlace {
    pub(super) local: String,
    pub(super) load: sv::ir::Expr,
    pub(super) store: sv::ir::Expr,
    pub(super) guards: Vec<sv::ir::Expr>,
}
