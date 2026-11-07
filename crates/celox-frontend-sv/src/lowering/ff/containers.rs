//! Procedural container operations use the same heap ABI on every backend.
use super::*;
use celox_heap::{containers as heap, ops::encode};
use sv::ir::{ContainerKind, ContainerLayout, ContainerOp, HandleProperty};

impl Ff<'_, '_> {
    /// The heap's parameter for a container of `layout`.
    pub(super) fn container_parameter(&self, layout: ContainerLayout) -> u64 {
        let (kind, capacity, signed_key, string_key, object_key) = match layout.kind {
            ContainerKind::Dynamic => (0, 0, false, false, false),
            ContainerKind::Queue { capacity } => (1, capacity, false, false, false),
            ContainerKind::Associative {
                signed,
                string,
                object,
                ..
            } => (2, 0, signed, string || wide_key(layout).is_some(), object),
        };
        let wide = if wide_key(layout).is_some() {
            heap::WIDE_KEY
        } else {
            0
        };
        wide | heap::parameter(
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
        )
    }

    /// Clamp arbitrary integral bounds before the signed 64-bit heap ABI.
    /// Negative bounds need only retain their sign; positive overflow is past
    /// every possible queue element. Unknown bounds make the range empty.
    fn queue_slice_bound(
        &mut self,
        expr: &sv::ir::Expr,
        lower: bool,
    ) -> Result<RegisterId, sv::AnalyzerError> {
        use sv::ir::{BinaryOp as B, Expr as E, UnaryOp as U};
        let value = self.heap_operand(expr, None, None)?;
        let literal = |text: &str| E::Literal(text.into());
        let binary = |left: E, op, right: E| E::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
        let mux = |condition: E, then_expr: E, else_expr: E| E::Mux {
            condition: Box::new(condition),
            then_expr: Box::new(then_expr),
            else_expr: Box::new(else_expr),
        };
        let max = literal("64'sh7fffffffffffffff");
        let negative = literal("64'shffffffffffffffff");
        let mut normalized = mux(
            binary(value.clone(), B::Gt, max.clone()),
            max.clone(),
            E::Resize {
                expr: Box::new(value.clone()),
                width: 64,
                signed: self.expr_signed(&value),
            },
        );
        if self.expr_signed(&value) {
            normalized = mux(
                binary(value.clone(), B::Lt, literal("64'sd0")),
                negative.clone(),
                normalized,
            );
        }
        let unknown = binary(
            E::Unary {
                op: U::RedXor,
                expr: Box::new(value),
            },
            B::EqCase,
            literal("1'bx"),
        );
        self.integer_of_width(
            &mux(unknown, if lower { max } else { negative }, normalized),
            64,
            true,
        )
    }

    pub(super) fn container_call(
        &mut self,
        op: ContainerOp,
        layout: ContainerLayout,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use ContainerOp::*;
        if op == Conditional {
            return self.container_conditional(layout, args);
        }
        if let Fixed { count } = op {
            return self.container_fixed(layout, count, args);
        }
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
            let count = args.len();
            // An operation that makes a value reads its operands as values; any
            // other updates or reads the container of its first operand in place.
            let value_receiver = operand(op, 0, count) != Operand::Receiver;
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
            for (index, arg) in args.iter_mut().enumerate() {
                let (context, kind) = match operand(op, index, count) {
                    Operand::Receiver => continue,
                    Operand::Element => (
                        Some((layout.width as usize, layout.signed)),
                        layout.handle.map(|h| match h {
                            HandleProperty::String => celox_design::ValueKind::StrRef,
                            HandleProperty::Object => celox_design::ValueKind::ObjRef,
                        }),
                    ),
                    Operand::Container => {
                        (Some((32, false)), Some(celox_design::ValueKind::ObjRef))
                    }
                    Operand::Size => (Some((64, true)), None),
                    Operand::Bound => (None, None),
                    Operand::Key => match layout.kind {
                        ContainerKind::Associative { string: true, .. } => {
                            (Some((32, false)), Some(celox_design::ValueKind::StrRef))
                        }
                        ContainerKind::Associative {
                            key_width, signed, ..
                        } => (Some((key_width as usize, signed)), None),
                        _ if matches!(op, Get | Set) => (Some((64, true)), None),
                        _ => (Some((32, true)), None),
                    },
                };
                *arg = self.heap_operand(arg, context, kind)?;
            }
        }
        let (signed_key, string_key) = match layout.kind {
            ContainerKind::Associative { signed, string, .. } => (signed, string),
            ContainerKind::Dynamic | ContainerKind::Queue { .. } => (false, false),
        };
        let wide = wide_key(layout);
        let parameter = self.container_parameter(layout);
        let code = |opcode| encode(opcode, parameter);
        if op.reduction().is_some() {
            return self.container_reduction(op, layout, &args[0], parameter);
        }
        // Evaluate the owning object once, before argument side effects. Read
        // its current container after the arguments, so a nested pop is kept.
        let property = if !matches!(
            op,
            New | Literal
                | WithDefault
                | Convert
                | Concat
                | Slice
                | Bound { .. }
                | Equal
                | CaseEqual
        ) && let sv::ir::Expr::Call {
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
        // Elements that are handles may be inner containers, whose own
        // elements can make an equality unknown (IEEE 1800-2023 11.4.5).
        let four_state = (element_result && !layout.two_state
            || op == Equal && (!layout.two_state || layout.handle == Some(HandleProperty::Object)))
            && self.m.four_state;
        let (temp, name) = self.m.temp("container", width, signed, four_state);
        if matches!(
            op,
            New | Literal
                | Keyed { .. }
                | WithDefault
                | Convert
                | Concat
                | Slice
                | Bound { .. }
                | Merge
        ) || element_result && layout.handle == Some(HandleProperty::Object)
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
            Slice => {
                let source = self.handle(&args[0])?;
                let lo = self.queue_slice_bound(&args[1], true)?;
                let hi = self.queue_slice_bound(&args[2], false)?;
                self.heap_call(code(heap::SLICE), &[source, lo, hi])
            }
            Keyed { default } => {
                // An associative array pattern: its default, then each key
                // with its value, a later key replacing an earlier one
                // (IEEE 1800-2023 10.9.1). A key with X or Z bits names no
                // entry, so its write is ignored (7.8.6).
                let (pairs, mut handle) = if default {
                    let (value, mask) = self.container_element(&args[0], layout)?;
                    (
                        &args[1..],
                        self.heap_call(code(heap::WITH_DEFAULT), &[zero, value, mask]),
                    )
                } else {
                    (&args[..], zero)
                };
                for pair in pairs.chunks(2) {
                    let (index, index_mask) = self.container_index(&pair[0], layout, Set)?;
                    let (value, mask) = self.container_element(&pair[1], layout)?;
                    let set = self.heap_call(code(heap::SET), &[handle, index, value]);
                    let set = self.heap_call(code(heap::SET_MASK), &[set, index, mask]);
                    let valid = self.b.alloc_bit(1, false);
                    self.b.emit(SIRInstruction::Binary(
                        valid,
                        index_mask,
                        BinaryOp::Eq,
                        zero,
                    ));
                    let next = self.b.alloc_bit(64, false);
                    self.b.emit(SIRInstruction::Mux(next, valid, set, handle));
                    handle = next;
                }
                handle
            }
            Merge => {
                let first = self.handle(&args[0])?;
                let second = self.handle(&args[1])?;
                self.heap_call(code(heap::MERGE), &[first, second])
            }
            Bound { depth } => {
                let source = self.handle(&args[0])?;
                let depth = self.constant64(u64::from(depth));
                self.heap_call(code(heap::BOUND), &[source, depth])
            }
            Concat => {
                let first = self.handle(&args[0])?;
                let second = self.handle(&args[1])?;
                self.heap_call(code(heap::CONCAT), &[first, second])
            }
            Convert => {
                let source = self.handle(&args[0])?;
                let size = self.heap_call(code(heap::LEN), &[source]);
                self.heap_call(code(heap::NEW), &[size, source])
            }
            WithDefault => {
                let (value, mask) = self.container_element(&args[0], layout)?;
                self.heap_call(code(heap::WITH_DEFAULT), &[zero, value, mask])
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
                    Shuffle => {
                        let seed = self.randomize_word()?;
                        self.integer_of_width(&seed, 64, false)?
                    }
                    Sort | Rsort => self.constant64(u64::from(layout.signed)),
                    _ => {
                        let (index, mask) = self.container_index(&args[1], layout, op)?;
                        index_mask = mask;
                        index
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
                        let mut key = this.heap_call(code(traversal), &[handle, index]);
                        let key_width = if string_key {
                            celox_design::HANDLE_WIDTH
                        } else if let Some((width, signed)) = wide {
                            key = this.wide_key_value(key, width, signed);
                            width
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
                                Shuffle => heap::SHUFFLE,
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
                                this.property_stored(&field);
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

    /// The fixed-size unpacked array of `count` elements that the container
    /// in `args[0]` is assigned, read by `args[1]` (see
    /// [`ContainerOp::Fixed`]): the container is evaluated once and checked
    /// to have `count` elements, and the reads take the checked handle.
    fn container_fixed(
        &mut self,
        layout: ContainerLayout,
        count: u32,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let [Some(source), Some(elements)] = args else {
            return Err(unsupported("fixed-size array of a container"));
        };
        let hoisted = self.hoist(source)?;
        let handle = self.handle(&hoisted)?;
        let count = self.constant64(u64::from(count));
        let code = encode(heap::SIZED, self.container_parameter(layout));
        let checked = self.heap_call(code, &[handle, count]);
        let (temp, name) = self.m.temp("fixed_source", 32, false, false);
        self.m.variables.get_mut(&temp).unwrap().value_kind = celox_design::ValueKind::ObjRef;
        let narrow = self.b.alloc_bit(32, false);
        self.b.emit(SIRInstruction::Slice(narrow, checked, 0, 32));
        self.store(temp, SIROffset::Static(0), 32, narrow);
        let elements = replace_expr(elements, source, &sv::ir::Expr::Ident(name));
        self.hoist(&elements)
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

    /// The [`heap::WIDE_KEY`] key of index `expr` of a `width`-bit index
    /// type, and whether it has unknown bits, which make it invalid (IEEE
    /// 1800-2023 7.8.4).
    /// A conditional operator of container operands, [`ContainerOp::Conditional`]:
    /// a known condition selects the operand it evaluates; an ambiguous one
    /// evaluates both and takes their merge (IEEE 1800-2023 11.4.11).
    fn container_conditional(
        &mut self,
        layout: ContainerLayout,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let [
            Some(sv::ir::Expr::Mux {
                condition,
                then_expr,
                else_expr,
            }),
        ] = args
        else {
            return Err(unsupported("container conditional operand"));
        };
        let condition = self.hoist(condition)?;
        let (then_value, else_value) = self.mux_arms(
            &condition,
            &mut (),
            |this, ()| this.hoist(then_expr),
            |this, ()| this.hoist(else_expr),
        )?;
        let (temp, name) = self.m.temp("container", 32, false, false);
        self.m.variables.get_mut(&temp).unwrap().value_kind = celox_design::ValueKind::ObjRef;
        let (truth, unknown) = if self.m.four_state {
            let mut arena = SLTNodeArena::new();
            let mut consts = ConstCache::default();
            let node = self.expr_slt(&condition, None, &mut arena)?;
            let truth = slt_truth(&mut arena, node)?;
            let unknown = slt_truth_unknown(&mut arena, &mut consts, node)?;
            (
                self.lower_slt(&arena, truth)?,
                Some(self.lower_slt(&arena, unknown)?),
            )
        } else {
            (self.eval_truth(&condition)?.0, None)
        };
        let then_handle = self.handle(&then_value)?;
        let else_handle = self.handle(&else_value)?;
        let picked = self.b.alloc_bit(64, false);
        self.b
            .emit(SIRInstruction::Mux(picked, truth, then_handle, else_handle));
        self.store(temp, SIROffset::Static(0), 32, picked);
        if let Some(unknown) = unknown {
            let code = encode(heap::MERGE, self.container_parameter(layout));
            self.when(unknown, |this| {
                let merged = this.heap_call(code, &[then_handle, else_handle]);
                this.store(temp, SIROffset::Static(0), 32, merged);
                Ok(())
            })?;
        }
        Ok(sv::ir::Expr::Ident(name))
    }

    /// The heap index of key or position `key` of container `layout` for
    /// operation `op`, with the X/Z mask of its value.
    fn container_index(
        &mut self,
        key: &sv::ir::Expr,
        layout: ContainerLayout,
        op: ContainerOp,
    ) -> Result<(RegisterId, RegisterId), sv::AnalyzerError> {
        use ContainerOp::*;
        let zero = self.constant64(0);
        if let ContainerKind::Associative { string: true, .. } = layout.kind {
            return Ok((self.handle(key)?, zero));
        }
        if let Some((width, signed)) = wide_key(layout) {
            return self.wide_key(key, width, signed);
        }
        let (width, signed) = match layout.kind {
            ContainerKind::Associative {
                key_width, signed, ..
            } => (key_width as usize, signed),
            _ if matches!(op, Get | Set) => (64, true),
            _ => (32, true),
        };
        let value = self.hoist(key)?;
        let reg = self.eval(&value, Some((width, signed)))?;
        let (raw, mask) = self.container_split(reg);
        let narrow = self.b.alloc_bit(width, signed);
        self.b.emit(SIRInstruction::Slice(narrow, raw, 0, width));
        let index = self.b.alloc_bit(64, false);
        self.b
            .emit(SIRInstruction::Unary(index, UnaryOp::Ident, narrow));
        if signed && width < 64 {
            let shift = self.constant64((64 - width) as u64);
            let shifted = self.b.alloc_bit(64, false);
            self.b
                .emit(SIRInstruction::Binary(shifted, index, BinaryOp::Shl, shift));
            let extended = self.b.alloc_bit(64, false);
            self.b.emit(SIRInstruction::Binary(
                extended,
                shifted,
                BinaryOp::Sar,
                shift,
            ));
            return Ok((extended, mask));
        }
        Ok((index, mask))
    }

    fn wide_key(
        &mut self,
        expr: &sv::ir::Expr,
        width: usize,
        signed: bool,
    ) -> Result<(RegisterId, RegisterId), sv::AnalyzerError> {
        let expr = self.hoist(expr)?;
        let reg = self.eval(&expr, Some((width, signed)))?;
        let logic = matches!(self.b.register(&reg), celox_sir::RegisterType::Logic { .. });
        let index = if logic {
            self.b.alloc_logic(width)
        } else {
            self.b.alloc_bit(width, signed)
        };
        self.b.emit(SIRInstruction::Slice(index, reg, 0, width));
        let (bits, unknown) = if logic {
            let bits = self.b.alloc_bit(width, signed);
            self.b
                .emit(SIRInstruction::Unary(bits, UnaryOp::ToTwoState, index));
            let known = self.b.alloc_bit(1, false);
            self.b
                .emit(SIRInstruction::Binary(known, index, BinaryOp::EqCase, bits));
            let unknown = self.b.alloc_bit(1, false);
            self.b
                .emit(SIRInstruction::Unary(unknown, UnaryOp::LogicNot, known));
            let wide = self.b.alloc_bit(64, false);
            self.b
                .emit(SIRInstruction::Unary(wide, UnaryOp::Ident, unknown));
            (bits, wide)
        } else {
            (index, self.constant64(0))
        };
        let mut buffer = self.heap_call(celox_heap::ops::NEW, &[]);
        for (low, high, bytes) in wide_key_chunks(width) {
            let part = self.b.alloc_bit(high - low, signed && high == width);
            self.b
                .emit(SIRInstruction::Slice(part, bits, low, high - low));
            let mut word = self.b.alloc_bit(64, false);
            self.b
                .emit(SIRInstruction::Unary(word, UnaryOp::Ident, part));
            if signed && high == width {
                if high - low < 64 {
                    let shift = self.constant64((64 - (high - low)) as u64);
                    let shifted = self.b.alloc_bit(64, false);
                    self.b
                        .emit(SIRInstruction::Binary(shifted, word, BinaryOp::Shl, shift));
                    word = self.b.alloc_bit(64, false);
                    self.b
                        .emit(SIRInstruction::Binary(word, shifted, BinaryOp::Sar, shift));
                }
                let top = self.constant64(1 << (bytes * 8 - 1));
                let flipped = self.b.alloc_bit(64, false);
                self.b
                    .emit(SIRInstruction::Binary(flipped, word, BinaryOp::Xor, top));
                word = flipped;
            }
            let count = self.constant64(bytes as u64);
            buffer = self.heap_call(celox_heap::ops::APPEND_BYTES, &[buffer, word, count]);
        }
        Ok((self.heap_call(celox_heap::ops::KEY, &[buffer]), unknown))
    }

    /// The `width`-bit index that [`heap::WIDE_KEY`] key `key` holds.
    fn wide_key_value(&mut self, key: RegisterId, width: usize, signed: bool) -> RegisterId {
        let mut offset = 0;
        let mut parts = Vec::new();
        for (low, high, bytes) in wide_key_chunks(width) {
            let start = self.constant64(offset);
            let count = self.constant64(bytes as u64);
            offset += bytes as u64;
            let mut word = self.heap_call(celox_heap::ops::KEY_BYTES, &[key, start, count]);
            if signed && high == width {
                let top = self.constant64(1 << (bytes * 8 - 1));
                let unflipped = self.b.alloc_bit(64, false);
                self.b
                    .emit(SIRInstruction::Binary(unflipped, word, BinaryOp::Xor, top));
                word = unflipped;
            }
            let part = self.b.alloc_bit(high - low, false);
            self.b
                .emit(SIRInstruction::Slice(part, word, 0, high - low));
            parts.push(part);
        }
        let value = self.b.alloc_bit(width, signed);
        self.b.emit(SIRInstruction::Concat(value, parts));
        value
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

/// The index type of an associative array of `layout` that is integral and
/// wider than 64 bits: its width and signedness.
/// How a container operation reads its operand `index` of `count`, for the
/// hoisting of calls in its operands: every operation is classified here, so
/// that a new one states how it reads each operand.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Operand {
    /// The container the operation updates or reads in place, or an output.
    Receiver,
    /// A container value.
    Container,
    /// An element value.
    Element,
    /// A key or a position.
    Key,
    /// The size of a new dynamic array.
    Size,
    /// A bound of a slice, in its own context.
    Bound,
}

fn operand(op: ContainerOp, index: usize, count: usize) -> Operand {
    use ContainerOp::*;
    match op {
        Literal | WithDefault => Operand::Element,
        // A keyed pattern's default comes first, then its keys and their
        // values in turn.
        Keyed { default } => {
            if default && index == 0 || (index + usize::from(default)) % 2 == 1 {
                Operand::Element
            } else {
                Operand::Key
            }
        }
        Merge
        | Conditional
        | Convert
        | Concat
        | Bound { .. }
        | Equal
        | CaseEqual
        | Fixed { .. } => Operand::Container,
        New if index == 0 => Operand::Size,
        New => Operand::Container,
        Slice if index == 0 => Operand::Container,
        Slice => Operand::Bound,
        First | Last | Next | Prev => Operand::Receiver,
        _ if index == 0 => Operand::Receiver,
        Set | Insert | PushFront | PushBack if index + 1 == count => Operand::Element,
        Shuffle | Reverse | Sort | Rsort | ReduceSum | ReduceProduct | ReduceAnd
        | LegacyReduceProduct | LegacyReduceAnd | ReduceOr | ReduceXor | Size | Get | Set
        | Clear | Delete | Insert | PushFront | PushBack | PopFront | PopBack | Exists => {
            Operand::Key
        }
    }
}

/// `expr` with each occurrence of `from` replaced by `to`.
fn replace_expr(expr: &sv::ir::Expr, from: &sv::ir::Expr, to: &sv::ir::Expr) -> sv::ir::Expr {
    use sv::ir::Expr;
    if expr == from {
        return to.clone();
    }
    let go = |expr: &Expr| Box::new(replace_expr(expr, from, to));
    match expr {
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: go(expr),
            msb: msb.clone(),
            lsb: lsb.clone(),
            signed: *signed,
        },
        Expr::Concat(parts) => Expr::Concat(
            parts
                .iter()
                .map(|part| replace_expr(part, from, to))
                .collect(),
        ),
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: go(expr),
            width: *width,
            signed: *signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op: *op,
            expr: go(expr),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: go(left),
            op: *op,
            right: go(right),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: go(condition),
            then_expr: go(then_expr),
            else_expr: go(else_expr),
        },
        Expr::Call {
            callee,
            args,
            arg_order,
        } => Expr::Call {
            callee: callee.clone(),
            args: args
                .iter()
                .map(|arg| arg.as_ref().map(|arg| replace_expr(arg, from, to)))
                .collect(),
            arg_order: arg_order.clone(),
        },
        _ => expr.clone(),
    }
}

fn wide_key(layout: ContainerLayout) -> Option<(usize, bool)> {
    match layout.kind {
        ContainerKind::Associative {
            key_width,
            signed,
            string: false,
            object: false,
        } if key_width > 64 => Some((key_width as usize, signed)),
        _ => None,
    }
}

/// The parts of a `width`-bit index in the bytes of its [`heap::WIDE_KEY`]
/// key, most significant first: the bits `low..high` of the index that each
/// part holds in its `bytes` bytes. The first part holds the bytes that the
/// index does not fill, so it extends the index to whole bytes.
fn wide_key_chunks(width: usize) -> Vec<(usize, usize, usize)> {
    let bytes = width.div_ceil(8);
    let mut chunks = Vec::new();
    let mut high = bytes * 8;
    let mut count = bytes - (bytes - 1) / 8 * 8;
    while high > 0 {
        let low = high - count * 8;
        chunks.push((low, high.min(width), count));
        high = low;
        count = 8;
    }
    chunks
}
