//! Writes to streams, concatenations and the places of class objects and containers.
use super::*;

impl BodyBuilder<'_, '_, '_> {
    /// An assignment to a streaming concatenation (IEEE 1800-2023 11.4.14.3),
    /// or `None` when the target is no stream. Fixed-size variables receive
    /// the source in one assignment; when some are dynamically sized, a
    /// hidden local holds the heap stream of the source, from which each
    /// variable takes its bits in turn.
    pub(super) fn stream_assignment(
        &mut self,
        op: &str,
        nonblocking: bool,
        left: celox_sv_syntax::ExprId,
        right: celox_sv_syntax::ExprId,
    ) -> Converted<Option<Vec<Stmt>>> {
        let native = self.tree.declarations();
        let Some(unpack) = expression_binding::stream_unpack(
            left,
            right,
            native,
            self.tree,
            self.session,
            &self.dims,
        )?
        else {
            return Ok(None);
        };
        if op != "=" {
            return Err(unsupported(
                "compound assignment to a streaming concatenation",
            ));
        }
        let mut stmts = match unpack {
            expression_binding::StreamUnpack::Fixed(parts, rhs) => vec![Stmt::AssignConcat {
                parts,
                rhs,
                nonblocking,
            }],
            expression_binding::StreamUnpack::Dynamic { source, targets } => {
                if nonblocking {
                    return Err(unsupported(
                        "nonblocking assignment to a streaming concatenation of dynamically sized data",
                    ));
                }
                let sink = self.dims.locator_locals.as_ref().ok_or_else(|| {
                    unsupported(
                        "streaming concatenation of dynamically sized data outside a procedural body",
                    )
                })?;
                let tokens = native.expression(left).tokens;
                let mut handle = Type::new(TypeKind::Bit);
                handle.packed_ranges.push(PackedRange::new(
                    ConstExpr::Literal("31".into()),
                    ConstExpr::Literal("0".into()),
                ));
                let name = sink.declare(
                    &format!("{}.{}.stream", tokens.start, tokens.end),
                    "stream",
                    handle,
                    self.tree,
                    self.session,
                    &self.dims,
                );
                let stream = Expr::Ident(name.clone());
                let mut stmts = vec![Stmt::Assign {
                    lhs: LValue::Ident(name),
                    rhs: source,
                    nonblocking: false,
                }];
                for (lhs, take) in targets {
                    let (rhs, read) = expression_binding::stream_take(&stream, take);
                    stmts.push(Stmt::Assign {
                        lhs,
                        rhs,
                        nonblocking: false,
                    });
                    stmts.extend(read.map(Stmt::Eval));
                }
                stmts.push(Stmt::Eval(expression_binding::stream_free(stream)));
                stmts
            }
        };
        // A class property or container element among the targets is
        // written through its object, as by any assignment, after the value
        // (4.9.3). The targets of a stream are bound before it is lowered,
        // so an address with effects, which must be evaluated once, is not
        // supported.
        let mut lowered = Vec::new();
        for stmt in std::mem::take(&mut stmts) {
            match stmt {
                Stmt::Assign {
                    lhs,
                    rhs,
                    nonblocking,
                } => {
                    if let LValue::Object { place, select, .. } = &lhs {
                        reject_effectful_address(place, select)?;
                    }
                    lowered.extend(self.emit_assign(lhs, rhs, nonblocking)?)
                }
                Stmt::AssignConcat {
                    parts,
                    rhs,
                    nonblocking,
                } if parts
                    .iter()
                    .any(|part| matches!(part, LValue::Object { .. })) =>
                {
                    lowered.extend(self.stream_places(left, parts, rhs, nonblocking)?)
                }
                mut stmt => {
                    self.rename_stream_stmt(&mut stmt);
                    lowered.push(stmt);
                }
            }
        }
        Ok(Some(lowered))
    }

    /// `parts = rhs`, the unpacking of streaming concatenation target
    /// `left`, with places among `parts`: the value into a local of the
    /// target's type, then each part its bits, from the most significant.
    pub(super) fn stream_places(
        &mut self,
        left: celox_sv_syntax::ExprId,
        parts: Vec<LValue>,
        mut rhs: Expr,
        nonblocking: bool,
    ) -> Converted<Vec<Stmt>> {
        let mut widths = Vec::new();
        for part in &parts {
            if let LValue::Object { place, select, .. } = part {
                reject_effectful_address(place, select)?;
            }
            let width = statements::lvalue_expr_type(&self.tree.functions, part, &self.dims)
                .map(|ty| ty.width)
                .ok_or_else(|| unsupported("width of a streaming concatenation target"))?;
            widths.push(width);
        }
        let total = widths.iter().sum::<usize>();
        let (source, unique) = self.hidden_of(left)?;
        self.rename_expr(&mut rhs);
        let mut stmts = vec![Stmt::Assign {
            lhs: LValue::Ident(unique),
            rhs,
            nonblocking: false,
        }];
        let mut offset = total;
        for (part, width) in parts.into_iter().zip(widths) {
            offset -= width;
            let bits = Expr::Select {
                expr: Box::new(Expr::Ident(source.clone())),
                msb: ConstExpr::Literal((offset + width - 1).to_string()),
                lsb: ConstExpr::Literal(offset.to_string()),
                signed: false,
            };
            stmts.extend(self.emit_assign(part, bits, nonblocking)?);
        }
        Ok(stmts)
    }

    pub(super) fn rename_stream_stmt(&self, stmt: &mut Stmt) {
        {
            match stmt {
                Stmt::Assign { lhs, rhs, .. } => {
                    self.rename_lvalue(lhs);
                    self.rename_expr(rhs);
                }
                Stmt::AssignConcat { parts, rhs, .. } => {
                    for part in parts {
                        self.rename_lvalue(part);
                    }
                    self.rename_expr(rhs);
                }
                Stmt::Eval(expr) => self.rename_expr(expr),
                _ => unreachable!("a stream assignment assigns and evaluates"),
            }
        }
    }

    /// `{targets} = right` with places among native targets `targets`, of
    /// concatenation `left`: the value is evaluated once, into a local of
    /// the concatenation's type, and each part, from the most significant,
    /// takes its bits as a write of its own (`target_write`), so two parts
    /// of one object see each other's writes in order (IEEE 1800-2023
    /// 4.9.3, 10.10.1).
    pub(super) fn concat_place_assignment(
        &mut self,
        left: celox_sv_syntax::ExprId,
        targets: Vec<celox_sv_syntax::ExprId>,
        right: celox_sv_syntax::ExprId,
        nonblocking: bool,
    ) -> Converted<Vec<Stmt>> {
        let native = self.tree.declarations();
        let mut widths = Vec::new();
        for &target in &targets {
            let lvalue = expression_binding::variable_lvalue(
                target,
                native,
                self.tree,
                self.session,
                &self.dims,
                false,
            )?;
            let width = statements::lvalue_expr_type(&self.tree.functions, &lvalue, &self.dims)
                .map(|ty| ty.width)
                .ok_or_else(|| unsupported("width of a concatenation target"))?;
            widths.push(width);
        }
        let total = widths.iter().sum::<usize>();
        let mut value =
            expression_binding::value(right, native, self.tree, self.session, &self.dims)?;
        // The value is extended to the targets' width as its signedness
        // gives (10.7, 11.8.2).
        let signed =
            dimensions::bound_bits_type(self.tree, &value, &self.dims).is_some_and(|ty| ty.signed);
        value = Expr::Resize {
            expr: Box::new(value),
            width: total,
            signed,
        };
        self.rename_expr(&mut value);
        let (source, unique) = self.hidden_of(left)?;
        let mut stmts = vec![Stmt::Assign {
            lhs: LValue::Ident(unique),
            rhs: value,
            nonblocking: false,
        }];
        let saved = (
            self.dims.effect_values.clone(),
            self.dims.last_indices.clone(),
        );
        let mut offset = total;
        for (target, width) in targets.into_iter().zip(widths) {
            offset -= width;
            let bits = Expr::Select {
                expr: Box::new(Expr::Ident(source.clone())),
                msb: ConstExpr::Literal((offset + width - 1).to_string()),
                lsb: ConstExpr::Literal(offset.to_string()),
                signed: false,
            };
            stmts.extend(self.target_write(target, bits, nonblocking)?);
        }
        (self.dims.effect_values, self.dims.last_indices) = saved;
        Ok(stmts)
    }

    /// The statements of a write of `value`, evaluated already and before
    /// renaming, to native target `target`, whose address is evaluated
    /// now, once, with the values it has when it is written (IEEE
    /// 1800-2023 4.9.3): a place through `place_assignment`.
    pub(super) fn target_write(
        &mut self,
        target: celox_sv_syntax::ExprId,
        value: Expr,
        nonblocking: bool,
    ) -> Converted<Vec<Stmt>> {
        let mut stmts = Vec::new();
        self.save_destination(target, false, &mut stmts)?;
        let (written, _) = self.native_lvalue(target, false)?;
        stmts.extend(self.emit_assign(written, value, nonblocking)?);
        Ok(stmts)
    }

    /// Evaluate the operands of the address of native target `target` that
    /// its write reads once into hidden locals, which later bindings of
    /// them read (`destination_operands`): the objects the write goes
    /// through and the indices with effects, or, with `all`, every index,
    /// as an assignment operator, which reads its target before it writes
    /// it, evaluates the address once (IEEE 1800-2023 11.4.1).
    pub(super) fn save_destination(
        &mut self,
        target: celox_sv_syntax::ExprId,
        all: bool,
        stmts: &mut Vec<Stmt>,
    ) -> Converted<()> {
        let (receivers, mut indices, lasts) = self.destination_operands(target, all);
        indices.extend(receivers);
        self.save_operands(indices, stmts)?;
        // The index that `$` names, of the queue the saved operands select,
        // innermost first.
        for select in lasts.into_iter().rev() {
            // Of an element of any type: integral, a structure, a record.
            let Some(index) =
                containers::element_index(select, self.tree, self.session, &self.dims)?
            else {
                continue;
            };
            let element = select;
            let (source, unique) = self.hidden(int_type());
            let mut index = index;
            self.rename_expr(&mut index);
            stmts.push(Stmt::Assign {
                lhs: LValue::Ident(unique),
                rhs: index,
                nonblocking: false,
            });
            Arc::make_mut(&mut self.dims.last_indices).insert(element, Expr::Ident(source));
        }
        Ok(())
    }

    /// The statements of `place.select = rhs`, a write of a class property
    /// or a container element, or a selection of its value, whose value
    /// `rhs` and address were evaluated, before renaming: the value at the
    /// place is read into a local of its type, the local's selection
    /// assigned, and the local written back to the place. Each write loads
    /// and stores on its own, so two places of one object in a statement,
    /// or a place and a read of it, see each other's writes in order.
    /// `guards`, the checks of the tags the selection crosses, are the
    /// place's: they read the value at the place when it is written,
    /// whatever form the write takes (IEEE 1800-2023 7.3.2). The local is
    /// of the type of `whole`, the native expression of the place's value.
    pub(super) fn place_assignment(
        &mut self,
        mut place: ObjectPlace,
        mut select: LValue,
        guards: Vec<Expr>,
        whole: celox_sv_syntax::ExprId,
        mut rhs: Expr,
        nonblocking: bool,
    ) -> Converted<Vec<Stmt>> {
        // IEEE 1800-2023 10.4.2 schedules the update of an object; it is not
        // implemented.
        if nonblocking {
            return Err(unsupported(
                "nonblocking assignment to a class property or container element",
            ));
        }
        if matches!(&select, LValue::Ident(name) if name == OBJECT_VALUE) {
            debug_assert!(guards.is_empty(), "a whole place crosses no member");
            self.rename_expr(&mut rhs);
            for expr in place.exprs_mut() {
                self.rename_expr(expr);
            }
            return Ok(vec![place.store(rhs)]);
        }
        let (local, unique) = self.hidden_of(whole)?;
        let mut rhs = packed_structs::checked_value(guards, rhs, self.tree, &self.dims)?;
        retarget_object_selection(&mut select, &local);
        // The checks of the tags the selection crosses read the local.
        rename_expr_idents(&mut rhs, &mut |name| {
            if name == OBJECT_VALUE {
                *name = local.clone();
            }
            None
        });
        self.rename_lvalue(&mut select);
        self.rename_expr(&mut rhs);
        for expr in place.exprs_mut() {
            self.rename_expr(expr);
        }
        Ok(vec![
            Stmt::Assign {
                lhs: LValue::Ident(unique.clone()),
                rhs: place.load(),
                nonblocking: false,
            },
            Stmt::Assign {
                lhs: select,
                rhs,
                nonblocking: false,
            },
            place.store(Expr::Ident(unique)),
        ])
    }

    /// `e.s = v` for a member `s` of unpacked structure type of a container
    /// element `e`: the flat value of the structure (7.2) is held in a
    /// vector local while the element is written with it. An operator
    /// assignment to an unpacked structure was rejected already.
    pub(super) fn element_struct_member_assignment(
        &mut self,
        nonblocking: bool,
        left: celox_sv_syntax::ExprId,
        right: celox_sv_syntax::ExprId,
    ) -> Converted<Option<Vec<Stmt>>> {
        let native = self.tree.declarations();
        let Some((ty, _)) =
            containers::element_member(left, native, self.tree, self.session, &self.dims)?
        else {
            return Ok(None);
        };
        let Some(structure) = ty
            .unpacked_struct()
            .filter(|_| ty.unpacked_ranges.is_empty())
        else {
            return Ok(None);
        };
        if nonblocking {
            return Err(unsupported(
                "nonblocking assignment to a class property or container element",
            ));
        }
        let width = structure
            .leaves()
            .iter()
            .map(|leaf| leaf.offset + leaf.width)
            .max()
            .unwrap_or(0);
        let id = unpacked_structs::type_id(&ty, self.tree, self.session, &self.dims)?;
        unpacked_structs::check_value(id, right, native, self.tree, self.session, &self.dims)?;
        let shape = dimensions_from_type(&ty);
        let mut value = expression_binding::bind_value(
            right,
            native,
            self.tree,
            self.session,
            &self.dims,
            Some(&shape),
        )?;
        self.rename_expr(&mut value);
        let mut local = Type::new(TypeKind::Logic);
        local.packed_ranges = vec![PackedRange::new(
            ConstExpr::Literal((width - 1).to_string()),
            ConstExpr::Literal("0".into()),
        )];
        let (source, unique) = self.hidden(local);
        let saved = (
            self.dims.effect_values.clone(),
            self.dims.last_indices.clone(),
        );
        let mut stmts = vec![Stmt::Assign {
            lhs: LValue::Ident(unique),
            rhs: value,
            nonblocking: false,
        }];
        self.save_destination(left, false, &mut stmts)?;
        let (written, _) = self.native_lvalue(left, true)?;
        stmts.extend(self.emit_assign(written, Expr::Ident(source), false)?);
        (self.dims.effect_values, self.dims.last_indices) = saved;
        Ok(Some(stmts))
    }

    pub(super) fn aggregate_property_assignment(
        &mut self,
        op: &str,
        nonblocking: bool,
        left: celox_sv_syntax::ExprId,
        right: celox_sv_syntax::ExprId,
    ) -> Converted<Option<Vec<Stmt>>> {
        let native = self.tree.declarations();
        let Some((aggregate, class, receiver)) = expression_binding::object_aggregate_property(
            self.tree,
            self.session,
            left,
            native,
            &self.dims,
        )?
        else {
            return Ok(None);
        };
        if nonblocking {
            return Err(unsupported("nonblocking assignment to a class property"));
        }
        if op != "=" {
            return Err(unsupported(format!(
                "operator `{op}` on an unpacked structure"
            )));
        }
        let mut handle = expression_binding::object_receiver(
            self.tree,
            self.session,
            receiver,
            native,
            &self.dims,
        )?;
        self.rename_expr(&mut handle);
        self.aggregate_store(&aggregate, &class, handle, right)
            .map(Some)
    }

    /// Store the value of native `right` into each leaf property of
    /// `aggregate` of the object `handle`.
    pub(super) fn aggregate_store(
        &mut self,
        aggregate: &classes::AggregateProperty,
        class: &classes::Class,
        handle: Expr,
        right: celox_sv_syntax::ExprId,
    ) -> Converted<Vec<Stmt>> {
        let native = self.tree.declarations();
        let structure = aggregate
            .r#type
            .unpacked_struct()
            .expect("an unpacked structure property")
            .clone();
        let structure_type =
            unpacked_structs::type_id(&aggregate.r#type, self.tree, self.session, &self.dims)?;
        unpacked_structs::check_value(
            structure_type,
            right,
            native,
            self.tree,
            self.session,
            &self.dims,
        )?;
        let mut value = expression_binding::bind_value(
            right,
            native,
            self.tree,
            self.session,
            &self.dims,
            Some(&dimensions_from_type(&aggregate.r#type)),
        )?;
        self.rename_expr(&mut value);
        let leaves = structure.leaves();
        let mut body = Vec::new();
        // Evaluate the whole value, which may read the members it replaces,
        // and the object it is stored in once, before any member is written.
        let mut flat = aggregate.r#type.clone();
        flat.unpacked_struct = None;
        let temp = self.declare("$aggregate", flat);
        body.push(Stmt::Local {
            name: temp.clone(),
            init: None,
        });
        body.push(Stmt::Assign {
            lhs: LValue::Ident(temp.clone()),
            rhs: value,
            nonblocking: false,
        });
        let handle = if inlining::calls_subroutine(&handle) {
            let receiver = self.declare("$receiver", classes::handle(&class.name));
            body.push(Stmt::Local {
                name: receiver.clone(),
                init: None,
            });
            body.push(Stmt::Assign {
                lhs: LValue::Ident(receiver.clone()),
                rhs: handle,
                nonblocking: false,
            });
            Expr::Ident(receiver)
        } else {
            handle
        };
        let parts = leaves.iter().map(|leaf| Expr::Select {
            expr: Box::new(Expr::Ident(temp.clone())),
            msb: ConstExpr::Literal((leaf.offset + leaf.width - 1).to_string()),
            lsb: ConstExpr::Literal(leaf.offset.to_string()),
            signed: false,
        });
        for (leaf, part) in leaves.iter().zip(parts) {
            let property = class
                .property(&leaf.name(&aggregate.name))
                .ok_or_else(|| unsupported("unpacked structure property member"))?;
            body.push(Stmt::Eval(classes::object_call(
                system_functions::ObjectOp::Store(property.field),
                vec![handle.clone(), part],
            )));
        }
        Ok(body)
    }
}

/// The local that a select of a dynamic container element is assigned in.
/// The selection `select` of `OBJECT_VALUE`, as the same selection of the
/// local `local` that holds the value.
pub(super) fn retarget_object_selection(select: &mut LValue, local: &str) {
    match select {
        LValue::Ident(name) | LValue::Select { name, .. } => *name = local.to_owned(),
        LValue::Object { .. } => lowered_object(),
    }
}

/// Reject a place bound without its native target, whose address would be
/// evaluated where it is read and again where it is written: one whose
/// object, container, index or select position has an effect.
pub(super) fn reject_effectful_address(place: &ObjectPlace, select: &LValue) -> Converted<()> {
    let mut place = place.clone();
    let positions = match select {
        LValue::Select {
            msb,
            lsb,
            array_slice_width,
            ..
        } => [Some(msb), Some(lsb), array_slice_width.as_ref()]
            .into_iter()
            .flatten()
            .any(inlining::effectful_position),
        _ => false,
    };
    if positions
        || place
            .exprs_mut()
            .into_iter()
            .any(|expr| inlining::calls_subroutine(expr) || inlining::effectful(expr))
    {
        return Err(unsupported(
            "streaming concatenation into a place selected with a side effect",
        ));
    }
    Ok(())
}

/// Reject a class property or container element that a statement assigns
/// without writing it through its object (`BodyBuilder::emit_assign`): no
/// pass after the body builder writes a place (`lowered_object`), so a form
/// of write that does not lower its places is unsupported.
pub(in crate::ast) fn reject_escaped_places(stmts: &[Stmt]) -> Converted<()> {
    let mut escaped = false;
    for stmt in stmts {
        stmt.walk(&mut |stmt| {
            escaped |= match stmt {
                Stmt::Assign { lhs, .. } => matches!(lhs, LValue::Object { .. }),
                Stmt::AssignConcat { parts, .. } => parts
                    .iter()
                    .any(|part| matches!(part, LValue::Object { .. })),
                _ => false,
            };
        });
    }
    if escaped {
        return Err(unsupported(
            "this form of write of a class property or container element",
        ));
    }
    Ok(())
}
