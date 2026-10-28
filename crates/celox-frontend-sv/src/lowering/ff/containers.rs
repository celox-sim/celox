//! Procedural container operations use the same heap ABI on every backend.
use super::*;
use celox_heap::{containers as heap, ops::encode};
use sv::ir::{ContainerKind, ContainerLayout, ContainerOp, HandleProperty};

impl Ff<'_, '_> {
    pub(super) fn container_call(
        &mut self,
        op: ContainerOp,
        layout: ContainerLayout,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use ContainerOp::*;
        let mut args = args
            .iter()
            .map(|arg| {
                arg.clone()
                    .ok_or_else(|| unsupported("omitted container operand"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        // An update of an element of an outer container reads that element
        // and writes it back: evaluate the indices on the way once.
        if op.mutates()
            && let Some(receiver) = args.first_mut()
        {
            *receiver = self.freeze_receiver(receiver)?;
        }
        let original_key_width = args
            .get(1)
            .and_then(|key| self.m.comparison_width(key, key));
        if op.reduction().is_none() && args.iter().any(|arg| self.m.calls(arg)) {
            let value_receiver = matches!(op, New | Literal | Convert | Equal | CaseEqual);
            if !value_receiver {
                // Preserve the receiver's location, not an old immutable copy
                // of its contents: arguments may mutate this same container.
                if let sv::ir::Expr::Call {
                    callee: sv::ir::Callee::Object(object),
                    args: operands,
                    ..
                } = &mut args[0]
                    && matches!(**object, sv::ir::ObjectOp::Load(_))
                    && let Some(owner) = operands.first_mut().and_then(Option::as_mut)
                {
                    *owner = self.heap_operand(
                        owner,
                        Some((32, false)),
                        Some(celox_design::ValueKind::ObjRef),
                    )?;
                }
            }
            // An element of an outer container is written back through its
            // read, whose indices `freeze_receiver` has evaluated.
            if !value_receiver
                && self.m.calls(&args[0])
                && !matches!(&args[0], sv::ir::Expr::Call { callee: sv::ir::Callee::Object(op), .. } if matches!(**op, sv::ir::ObjectOp::Load(_) | sv::ir::ObjectOp::Container { op: Get, .. }))
            {
                args[0] = self.heap_operand(
                    &args[0],
                    Some((32, false)),
                    Some(celox_design::ValueKind::ObjRef),
                )?;
            }
            let count = args.len();
            for (index, arg) in args.iter_mut().enumerate() {
                if !value_receiver && (index == 0 || matches!(op, First | Last | Next | Prev)) {
                    continue;
                }
                let element = op == Literal
                    || (matches!(op, Set | Insert | PushFront | PushBack) && index + 1 == count);
                let (context, kind) = if element {
                    (
                        Some((layout.width as usize, layout.signed)),
                        layout.handle.map(|h| match h {
                            HandleProperty::String => celox_design::ValueKind::StrRef,
                            HandleProperty::Object => celox_design::ValueKind::ObjRef,
                        }),
                    )
                } else if op == New && index == 0 {
                    (Some((64, true)), None)
                } else if value_receiver {
                    (Some((32, false)), Some(celox_design::ValueKind::ObjRef))
                } else {
                    match layout.kind {
                        ContainerKind::Associative { string: true, .. } => {
                            (Some((32, false)), Some(celox_design::ValueKind::StrRef))
                        }
                        ContainerKind::Associative {
                            key_width, signed, ..
                        } => (Some((key_width as usize, signed)), None),
                        _ if matches!(op, Get | Set) => (Some((64, true)), None),
                        _ => (Some((32, true)), None),
                    }
                };
                *arg = self.heap_operand(arg, context, kind)?;
            }
        }
        let (kind, capacity, signed_key, string_key, object_key) = match layout.kind {
            ContainerKind::Dynamic => (0, 0, false, false, false),
            ContainerKind::Queue { capacity } => (1, capacity, false, false, false),
            ContainerKind::Associative {
                signed,
                string,
                object,
                ..
            } => (2, 0, signed, string, object),
        };
        let parameter = heap::parameter(
            layout.width,
            layout.two_state || !self.m.four_state,
            match layout.handle {
                None => 0,
                Some(HandleProperty::String) => 1,
                Some(HandleProperty::Object) => 2,
            },
            kind,
            capacity,
            signed_key,
            string_key,
            object_key,
        );
        let code = |opcode| encode(opcode, parameter);
        if op.reduction().is_some() {
            return self.container_reduction(op, layout, &args[0], parameter);
        }
        // Evaluate the owning object once, before argument side effects. Read
        // its current container after the arguments, so a nested pop is kept.
        let property = if !matches!(op, New | Literal | Convert | Equal | CaseEqual)
            && let sv::ir::Expr::Call {
                callee: sv::ir::Callee::Object(object),
                args,
                ..
            } = &args[0]
            && let sv::ir::ObjectOp::Load(field) = **object
        {
            let owner = args[0]
                .as_ref()
                .ok_or_else(|| unsupported("container property owner"))?;
            Some((field, self.handle(owner)?))
        } else {
            None
        };
        let zero = self.constant64(0);
        let (width, signed) = op.result_type(layout);
        let element_result = matches!(op, Get | PopFront | PopBack);
        let four_state = (element_result || op == Equal) && !layout.two_state && self.m.four_state;
        let (temp, name) = self.m.temp("container", width, signed, four_state);
        if matches!(op, New | Literal | Convert)
            || element_result && layout.handle == Some(HandleProperty::Object)
        {
            self.m.variables.get_mut(&temp).unwrap().value_kind = celox_design::ValueKind::ObjRef;
        } else if element_result && layout.handle == Some(HandleProperty::String) {
            self.m.variables.get_mut(&temp).unwrap().value_kind = celox_design::ValueKind::StrRef;
        }
        let result = match op {
            Equal | CaseEqual => {
                let left = self.handle(&args[0])?;
                let right = self.handle(&args[1])?;
                let raw = self.heap_call(
                    code(if op == Equal {
                        heap::EQUAL
                    } else {
                        heap::CASE_EQUAL
                    }),
                    &[left, right],
                );
                let one = self.constant64(1);
                let mask = self.b.alloc_bit(64, false);
                self.b
                    .emit(SIRInstruction::Binary(mask, raw, BinaryOp::Shr, one));
                let value = self.container_join(raw, mask, 1, four_state);
                self.store(temp, SIROffset::Static(0), 1, value);
                return Ok(sv::ir::Expr::Ident(name));
            }
            New => {
                let size = self.integer_of_width(&args[0], 64, true)?;
                let source = self.handle(&args[1])?;
                self.heap_call(code(heap::NEW), &[size, source])
            }
            Convert if matches!(layout.kind, ContainerKind::Associative { .. }) => {
                self.handle(&args[0])?
            }
            Convert => {
                let source = self.handle(&args[0])?;
                let size = self.heap_call(code(heap::LEN), &[source]);
                self.heap_call(code(heap::NEW), &[size, source])
            }
            Literal => {
                let size = self.constant64(args.len() as u64);
                let mut handle = self.heap_call(code(heap::NEW), &[size, zero]);
                for (i, arg) in args.iter().enumerate() {
                    let (value, mask) = self.container_element(arg, layout)?;
                    let index = self.constant64(i as u64);
                    handle = self.heap_call(code(heap::SET), &[handle, index, value]);
                    handle = self.heap_call(code(heap::SET_MASK), &[handle, index, mask]);
                }
                handle
            }
            _ => {
                let mut index_mask = zero;
                let index = match op {
                    Size | Clear | PushFront | PopFront | PushBack | PopBack | Reverse => zero,
                    Sort | Rsort => self.constant64(u64::from(layout.signed)),
                    _ if string_key => self.handle(&args[1])?,
                    _ => {
                        let (width, signed) = match layout.kind {
                            ContainerKind::Associative {
                                key_width, signed, ..
                            } => (key_width as usize, signed),
                            _ if matches!(op, Get | Set) => (64, true),
                            _ => (32, true),
                        };
                        let value = self.hoist(&args[1])?;
                        let reg = self.eval(&value, Some((width, signed)))?;
                        let (raw, mask) = self.container_split(reg);
                        index_mask = mask;
                        let narrow = self.b.alloc_bit(width, signed);
                        self.b.emit(SIRInstruction::Slice(narrow, raw, 0, width));
                        let index = self.b.alloc_bit(64, false);
                        self.b
                            .emit(SIRInstruction::Unary(index, UnaryOp::Ident, narrow));
                        if signed && width < 64 {
                            let shift = self.constant64((64 - width) as u64);
                            let shifted = self.b.alloc_bit(64, false);
                            self.b.emit(SIRInstruction::Binary(
                                shifted,
                                index,
                                BinaryOp::Shl,
                                shift,
                            ));
                            let extended = self.b.alloc_bit(64, false);
                            self.b.emit(SIRInstruction::Binary(
                                extended,
                                shifted,
                                BinaryOp::Sar,
                                shift,
                            ));
                            extended
                        } else {
                            index
                        }
                    }
                };
                let (value, mask) = if matches!(op, Set | Insert | PushFront | PushBack) {
                    self.container_element(args.last().unwrap(), layout)?
                } else {
                    (zero, zero)
                };
                // Arguments can mutate the receiver (e.g. q.push_back(q.pop_front())).
                // Read its current contents and derive positions only after evaluating them.
                let handle = if let Some((field, owner)) = property {
                    let offset = self.constant64(u64::from(field.offset));
                    let count = self.constant64(u64::from(field.bytes()));
                    self.heap_call(celox_heap::ops::OBJECT_LOAD, &[owner, offset, count])
                } else {
                    self.handle(&args[0])?
                };
                let index = match op {
                    PushBack => self.heap_call(code(heap::LEN), &[handle]),
                    PopBack => {
                        let size = self.heap_call(code(heap::LEN), &[handle]);
                        let one = self.constant64(1);
                        let index = self.b.alloc_bit(64, false);
                        self.b
                            .emit(SIRInstruction::Binary(index, size, BinaryOp::Sub, one));
                        index
                    }
                    _ => index,
                };
                if matches!(op, First | Last | Next | Prev) {
                    let traversal = match op {
                        First => heap::FIRST,
                        Last => heap::LAST,
                        Next => heap::NEXT,
                        _ => heap::PREV,
                    };
                    let traversal_reg = self.constant64(traversal);
                    let exists =
                        self.heap_call(code(heap::ITER_VALID), &[handle, index, traversal_reg]);
                    let exists = if matches!(op, Next | Prev) {
                        let valid = self.b.alloc_bit(1, false);
                        self.b.emit(SIRInstruction::Binary(
                            valid,
                            index_mask,
                            BinaryOp::Eq,
                            zero,
                        ));
                        let known = self.b.alloc_bit(64, false);
                        self.b.emit(SIRInstruction::Mux(known, valid, exists, zero));
                        known
                    } else {
                        exists
                    };
                    let target = args[1].clone();
                    self.when(exists, |this| {
                        let key = this.heap_call(code(traversal), &[handle, index]);
                        let key_width = if string_key {
                            celox_design::HANDLE_WIDTH
                        } else {
                            64
                        };
                        let (id, key_name) =
                            this.m.temp("container_key", key_width, signed_key, false);
                        if string_key {
                            this.m.variables.get_mut(&id).unwrap().value_kind =
                                celox_design::ValueKind::StrRef;
                        }
                        let value = this.b.alloc_bit(key_width, signed_key);
                        this.b.emit(SIRInstruction::Slice(value, key, 0, key_width));
                        this.store(id, SIROffset::Static(0), key_width, value);
                        let sv::ir::Expr::Ident(target) = target else {
                            return Err(unsupported("associative index output"));
                        };
                        this.assign(
                            &sv::ir::LValue::Ident(target),
                            &sv::ir::Expr::Ident(key_name),
                        )
                    })?;
                    let narrow = match layout.kind {
                        ContainerKind::Associative {
                            key_width,
                            string: false,
                            ..
                        } => original_key_width.is_some_and(|width| width < key_width as usize),
                        _ => false,
                    };
                    if narrow {
                        let minus_one = self.constant64(u64::MAX);
                        let status = self.b.alloc_bit(64, false);
                        self.b
                            .emit(SIRInstruction::Mux(status, exists, minus_one, zero));
                        status
                    } else {
                        exists
                    }
                } else if op == Size {
                    self.heap_call(code(heap::LEN), &[handle])
                } else if op == Exists {
                    let exists = self.heap_call(code(heap::EXISTS), &[handle, index]);
                    let valid = self.b.alloc_bit(1, false);
                    self.b.emit(SIRInstruction::Binary(
                        valid,
                        index_mask,
                        BinaryOp::Eq,
                        zero,
                    ));
                    let result = self.b.alloc_bit(64, false);
                    self.b
                        .emit(SIRInstruction::Mux(result, valid, exists, zero));
                    result
                } else {
                    if element_result {
                        let value = self.heap_call(code(heap::GET), &[handle, index, index_mask]);
                        let mask =
                            self.heap_call(code(heap::GET_MASK), &[handle, index, index_mask]);
                        let result = self.container_join(value, mask, width, four_state);
                        self.store(temp, SIROffset::Static(0), width, result);
                    }
                    if op.mutates() {
                        let valid = self.b.alloc_bit(1, false);
                        self.b.emit(SIRInstruction::Binary(
                            valid,
                            index_mask,
                            BinaryOp::Eq,
                            zero,
                        ));
                        let receiver = args[0].clone();
                        self.when(valid, |this| {
                            let opcode = match op {
                                Set => heap::SET,
                                Insert | PushFront | PushBack => heap::INSERT,
                                Clear => heap::CLEAR,
                                Reverse => heap::REVERSE,
                                Sort => heap::SORT,
                                Rsort => heap::RSORT,
                                _ => heap::DELETE,
                            };
                            let mut updated = this.heap_call(code(opcode), &[handle, index, value]);
                            if matches!(op, Set | Insert | PushFront | PushBack) {
                                updated =
                                    this.heap_call(code(heap::SET_MASK), &[updated, index, mask]);
                            }
                            if let Some((field, owner)) = property {
                                let place = this.constant64(
                                    u64::from(field.offset) | u64::from(field.bytes()) << 32,
                                );
                                this.heap_call(
                                    celox_heap::ops::OBJECT_STORE,
                                    &[owner, place, updated],
                                );
                                return Ok(());
                            }
                            let (id, updated_name) =
                                this.m.temp("container_update", 32, false, false);
                            this.m.variables.get_mut(&id).unwrap().value_kind =
                                celox_design::ValueKind::ObjRef;
                            let narrow = this.b.alloc_bit(32, false);
                            this.b.emit(SIRInstruction::Slice(narrow, updated, 0, 32));
                            this.store(id, SIROffset::Static(0), 32, narrow);
                            this.container_write_back(&receiver, sv::ir::Expr::Ident(updated_name))
                        })?;
                    }
                    zero
                }
            }
        };
        if !element_result {
            let narrow = self.b.alloc_bit(width, signed);
            self.b.emit(SIRInstruction::Slice(narrow, result, 0, width));
            self.store(temp, SIROffset::Static(0), width, narrow);
        }
        Ok(sv::ir::Expr::Ident(name))
    }

    /// `receiver` with the indices of the outer containers it reads, and the
    /// objects of their properties, evaluated into temporaries.
    fn freeze_receiver(
        &mut self,
        receiver: &sv::ir::Expr,
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let sv::ir::Expr::Call {
            callee: sv::ir::Callee::Object(op),
            args,
            arg_order,
        } = receiver
        else {
            return Ok(receiver.clone());
        };
        let frozen = match **op {
            sv::ir::ObjectOp::Container {
                op: ContainerOp::Get,
                ..
            } => {
                let [Some(outer), Some(index)] = args.as_slice() else {
                    return Ok(receiver.clone());
                };
                let outer = self.freeze_receiver(outer)?;
                let index = self.hoist(index)?;
                vec![Some(outer), Some(index)]
            }
            sv::ir::ObjectOp::Load(_) => {
                let [Some(owner)] = args.as_slice() else {
                    return Ok(receiver.clone());
                };
                vec![Some(self.hoist(owner)?)]
            }
            _ => return Ok(receiver.clone()),
        };
        Ok(sv::ir::Expr::Call {
            callee: sv::ir::Callee::Object(op.clone()),
            args: frozen,
            arg_order: arg_order.clone(),
        })
    }

    /// Store the updated container `updated` where `receiver` read it: a
    /// variable, or an element of an outer container (`m[a]` of `m[a][b]`),
    /// which is set in turn (IEEE 1800-2023 7.4.5).
    fn container_write_back(
        &mut self,
        receiver: &sv::ir::Expr,
        updated: sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        match receiver {
            sv::ir::Expr::Ident(name) => {
                self.assign(&sv::ir::LValue::Ident(name.clone()), &updated)
            }
            sv::ir::Expr::Call {
                callee: sv::ir::Callee::Object(op),
                args,
                ..
            } if matches!(
                **op,
                sv::ir::ObjectOp::Container {
                    op: ContainerOp::Get,
                    ..
                }
            ) =>
            {
                let sv::ir::ObjectOp::Container { layout, .. } = **op else {
                    unreachable!("matched a container read")
                };
                let [Some(outer), Some(index)] = args.as_slice() else {
                    return Err(unsupported("container element receiver"));
                };
                self.container_call(
                    ContainerOp::Set,
                    layout,
                    &[Some(outer.clone()), Some(index.clone()), Some(updated)],
                )?;
                Ok(())
            }
            _ => Err(unsupported("container receiver must be a variable")),
        }
    }

    fn container_reduction(
        &mut self,
        op: ContainerOp,
        layout: ContainerLayout,
        receiver: &sv::ir::Expr,
        parameter: u64,
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let width = layout.width as usize;
        let four_state = !layout.two_state && self.m.four_state;
        let alloc = |b: &mut SIRBuilder<Addr>| {
            if four_state {
                b.alloc_logic(width)
            } else {
                b.alloc_bit(width, layout.signed)
            }
        };
        let code = |opcode| encode(opcode, parameter);
        // Keep one immutable snapshot of the receiver throughout the traversal.
        let handle = self.handle(receiver)?;
        let zero = self.constant64(0);
        let one = self.constant64(1);
        let length = self.heap_call(code(heap::LEN), &[handle]);
        let associative = matches!(layout.kind, ContainerKind::Associative { .. });
        let first = if associative {
            self.heap_call(code(heap::FIRST), &[handle, zero])
        } else {
            zero
        };
        let seed = alloc(&mut self.b);
        let identity = match op {
            ContainerOp::ReduceProduct | ContainerOp::LegacyReduceProduct => BigUint::from(1u8),
            ContainerOp::ReduceAnd | ContainerOp::LegacyReduceAnd => {
                (BigUint::from(1u8) << width) - BigUint::from(1u8)
            }
            _ => BigUint::zero(),
        };
        self.b
            .emit(SIRInstruction::Imm(seed, SIRValue::new(identity)));
        let remaining = self.b.alloc_bit(64, false);
        let key = self.b.alloc_bit(64, false);
        let accumulator = alloc(&mut self.b);
        let header = self.b.new_block_with(vec![remaining, key, accumulator]);
        let body = self.b.new_block();
        let result = alloc(&mut self.b);
        let exit = self.b.new_block_with(vec![result]);
        self.b
            .seal_block(SIRTerminator::Jump(header, vec![length, first, seed]));
        self.b.switch_to_block(header);
        let more = self.b.alloc_bit(1, false);
        self.b
            .emit(SIRInstruction::Binary(more, remaining, BinaryOp::Ne, zero));
        self.b.seal_block(SIRTerminator::Branch {
            cond: more,
            true_block: (body, Vec::new()),
            false_block: (exit, vec![accumulator]),
        });
        self.b.switch_to_block(body);
        let raw = self.heap_call(code(heap::GET), &[handle, key, zero]);
        let mask = self.heap_call(code(heap::GET_MASK), &[handle, key, zero]);
        let element = self.container_join(raw, mask, width, four_state);
        let combined = alloc(&mut self.b);
        let binary = match op {
            ContainerOp::ReduceSum => BinaryOp::Add,
            ContainerOp::ReduceProduct | ContainerOp::LegacyReduceProduct => BinaryOp::Mul,
            ContainerOp::ReduceAnd | ContainerOp::LegacyReduceAnd => BinaryOp::And,
            ContainerOp::ReduceOr => BinaryOp::Or,
            ContainerOp::ReduceXor => BinaryOp::Xor,
            _ => unreachable!("reduction method"),
        };
        self.b.emit(SIRInstruction::Binary(
            combined,
            accumulator,
            binary,
            element,
        ));
        let next = if associative {
            self.heap_call(code(heap::NEXT), &[handle, key])
        } else {
            let next = self.b.alloc_bit(64, false);
            self.b
                .emit(SIRInstruction::Binary(next, key, BinaryOp::Add, one));
            next
        };
        let fewer = self.b.alloc_bit(64, false);
        self.b
            .emit(SIRInstruction::Binary(fewer, remaining, BinaryOp::Sub, one));
        self.b
            .seal_block(SIRTerminator::Jump(header, vec![fewer, next, combined]));
        self.b.switch_to_block(exit);
        // The accumulator starts at the operation's identity. Only explicit
        // legacy compatibility replaces an empty product/AND with zero.
        let value = if matches!(
            op,
            ContainerOp::LegacyReduceProduct | ContainerOp::LegacyReduceAnd
        ) {
            let empty = self.b.alloc_bit(1, false);
            self.b
                .emit(SIRInstruction::Binary(empty, length, BinaryOp::Eq, zero));
            let empty_value = alloc(&mut self.b);
            self.b
                .emit(SIRInstruction::Imm(empty_value, SIRValue::new(0u8)));
            let value = alloc(&mut self.b);
            self.b
                .emit(SIRInstruction::Mux(value, empty, empty_value, result));
            value
        } else {
            result
        };
        let (temp, name) = self
            .m
            .temp("array_reduction", width, layout.signed, four_state);
        self.store(temp, SIROffset::Static(0), width, value);
        Ok(sv::ir::Expr::Ident(name))
    }

    fn container_element(
        &mut self,
        expr: &sv::ir::Expr,
        layout: ContainerLayout,
    ) -> Result<(RegisterId, RegisterId), sv::AnalyzerError> {
        let expr = self.hoist(expr)?;
        let value = self.eval(&expr, Some((layout.width as usize, layout.signed)))?;
        if layout.two_state {
            let known = self.b.alloc_bit(layout.width as usize, layout.signed);
            self.b
                .emit(SIRInstruction::Unary(known, UnaryOp::ToTwoState, value));
            return Ok(self.container_split(known));
        }
        Ok(self.container_split(value))
    }

    // Case equality exposes the planes without changing the SIR or backend ABI.
    pub(super) fn container_split(&mut self, reg: RegisterId) -> (RegisterId, RegisterId) {
        let width = self.b.register(&reg).width();
        if !self.m.four_state
            || !matches!(self.b.register(&reg), celox_sir::RegisterType::Logic { .. })
        {
            let value = self.b.alloc_bit(64, false);
            self.b
                .emit(SIRInstruction::Unary(value, UnaryOp::Ident, reg));
            return (value, self.constant64(0));
        }
        let one = self.b.alloc_logic(1);
        self.b
            .emit(SIRInstruction::Imm(one, SIRValue::new(BigUint::from(1u8))));
        let z = self.b.alloc_logic(1);
        self.b.emit(SIRInstruction::Imm(
            z,
            SIRValue::new_four_state(BigUint::zero(), BigUint::from(1u8)),
        ));
        let x = self.b.alloc_logic(1);
        self.b.emit(SIRInstruction::Imm(
            x,
            SIRValue::new_four_state(BigUint::from(1u8), BigUint::from(1u8)),
        ));
        let mut values = Vec::new();
        let mut masks = Vec::new();
        for i in (0..width).rev() {
            let bit = self.b.alloc_logic(1);
            self.b.emit(SIRInstruction::Slice(bit, reg, i, 1));
            let is_one = self.b.alloc_bit(1, false);
            let is_z = self.b.alloc_bit(1, false);
            let is_x = self.b.alloc_bit(1, false);
            self.b
                .emit(SIRInstruction::Binary(is_one, bit, BinaryOp::EqCase, one));
            self.b
                .emit(SIRInstruction::Binary(is_z, bit, BinaryOp::EqCase, z));
            self.b
                .emit(SIRInstruction::Binary(is_x, bit, BinaryOp::EqCase, x));
            let value = self.b.alloc_bit(1, false);
            let mask = self.b.alloc_bit(1, false);
            self.b
                .emit(SIRInstruction::Binary(value, is_one, BinaryOp::Or, is_x));
            self.b
                .emit(SIRInstruction::Binary(mask, is_x, BinaryOp::Or, is_z));
            values.push(value);
            masks.push(mask);
        }
        let mut packed = Vec::new();
        for bits in [values, masks] {
            let narrow = self.b.alloc_bit(width, false);
            self.b.emit(SIRInstruction::Concat(narrow, bits));
            let wide = self.b.alloc_bit(64, false);
            self.b
                .emit(SIRInstruction::Unary(wide, UnaryOp::Ident, narrow));
            packed.push(wide);
        }
        (packed[0], packed[1])
    }

    pub(super) fn container_join(
        &mut self,
        value: RegisterId,
        mask: RegisterId,
        width: usize,
        four_state: bool,
    ) -> RegisterId {
        if !four_state {
            let reg = self.b.alloc_bit(width, false);
            self.b.emit(SIRInstruction::Slice(reg, value, 0, width));
            return reg;
        }
        let x = self.b.alloc_logic(1);
        let z = self.b.alloc_logic(1);
        self.b.emit(SIRInstruction::Imm(
            x,
            SIRValue::new_four_state(BigUint::from(1u8), BigUint::from(1u8)),
        ));
        self.b.emit(SIRInstruction::Imm(
            z,
            SIRValue::new_four_state(BigUint::zero(), BigUint::from(1u8)),
        ));
        let mut bits = Vec::new();
        for i in (0..width).rev() {
            let v = self.b.alloc_bit(1, false);
            let m = self.b.alloc_bit(1, false);
            self.b.emit(SIRInstruction::Slice(v, value, i, 1));
            self.b.emit(SIRInstruction::Slice(m, mask, i, 1));
            let unknown = self.b.alloc_logic(1);
            self.b.emit(SIRInstruction::Mux(unknown, v, x, z));
            let bit = self.b.alloc_logic(1);
            self.b.emit(SIRInstruction::Mux(bit, m, unknown, v));
            bits.push(bit);
        }
        let result = self.b.alloc_logic(width);
        self.b.emit(SIRInstruction::Concat(result, bits));
        result
    }
}
