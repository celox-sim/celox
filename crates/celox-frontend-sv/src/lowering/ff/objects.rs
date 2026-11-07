//! Class object operations (IEEE 1800-2023 clause 8), lowered to calls of
//! the heap helper.
//!
//! An object handle is 32 bits like a string handle. The analyzer has laid
//! out each class, so an operation carries the byte offset and width of the
//! property it reads or writes.

use super::*;
use celox_heap::ops;
use sv::ir::{HandleProperty, ObjectOp};

impl Ff<'_, '_> {
    /// Count a store of property `field` in its change count, if a level
    /// wait of the design reads it (see [`property_changes`]).
    pub(super) fn property_stored(&mut self, field: &sv::ir::ObjectField) {
        if let Some(id) = self.m.change_counter(field) {
            self.count_property_change(id);
        }
    }

    /// Whether an event control watches the property `field` (see
    /// `property_edges`): its stores count the occurrences of the object.
    fn edges_watched(&self, field: &sv::ir::ObjectField) -> bool {
        field.key != 0
            && self
                .m
                .id(&property_changes(property_edges(field.key)))
                .is_some()
    }

    /// Before a store of `field` into `object` that an event control
    /// watches, keep the property's bytes; returns whether it does.
    fn edges_begin(&mut self, object: RegisterId, field: &sv::ir::ObjectField) -> bool {
        if !self.edges_watched(field) {
            return false;
        }
        let place = self.constant64(u64::from(field.offset) | u64::from(field.storage()) << 32);
        self.heap_call(ops::OBJECT_EDGE_BEGIN, &[object, place]);
        true
    }

    /// After that store, count its change and the edge of its least
    /// significant bit in the object's occurrences (IEEE 1800-2023 9.4.2).
    fn edges_end(&mut self, object: RegisterId, field: &sv::ir::ObjectField, counted: bool) {
        if !counted {
            return;
        }
        let place = self.constant64(u64::from(field.offset) | u64::from(field.bytes()) << 32);
        let four_state = self.constant64(u64::from(field.four_state && self.m.four_state));
        self.heap_call(ops::OBJECT_EDGE_END, &[object, place, four_state]);
    }

    /// An event control whose items are properties of objects (IEEE
    /// 1800-2023 9.4.2), in a kernel or a unit: each occurrence is counted
    /// in the stored object at the store (see `edges_end`). The process
    /// samples, at the start of the wait, each item's object and its count
    /// of the item's occurrences, waits for a store of the property, which
    /// bumps the key's change count, or for a change of a handle read from
    /// module storage, and at each resume finds an occurrence when the
    /// object's count advanced, or, for an item whose handle now denotes
    /// another object, when the property's value changed by the item's edge
    /// from the old object's to the new one's. Returns whether `items` were
    /// such an event control.
    pub(super) fn property_event(
        &mut self,
        items: &[sv::ir::EventItem],
    ) -> Result<bool, sv::AnalyzerError> {
        let mut parts = Vec::new();
        for item in items {
            let expr = self.propagate(&item.expr);
            let sv::ir::Expr::Call {
                callee: sv::ir::Callee::Object(op),
                args,
                ..
            } = &expr
            else {
                return Ok(false);
            };
            let (ObjectOp::Load(field), [Some(handle)]) = (&**op, args.as_slice()) else {
                return Ok(false);
            };
            if item.iff.is_some() || !self.edges_watched(field) {
                return Ok(false);
            }
            parts.push((*field, handle.clone(), item.edge));
        }
        if parts.is_empty() {
            return Ok(false);
        }
        let mut terms = Vec::new();
        let mut samples = Vec::new();
        for (field, handle, edge) in &parts {
            self.check_event_purity(handle)?;
            let changes = self
                .m
                .id(&property_changes(field.key))
                .ok_or_else(|| unsupported("an unwatched property in an event control"))?;
            terms.push((changes, CounterKind::Changes));
            // A handle in the frame or in the process cannot change while
            // the process waits; one in module storage can.
            let moves = !self.level_terms(handle)?.is_empty();
            if moves {
                terms.extend(
                    self.level_terms(handle)?
                        .into_iter()
                        .map(|id| (id, CounterKind::Changes)),
                );
            }
            let (object_id, object) =
                self.m
                    .temp("event_object", celox_design::HANDLE_WIDTH, false, false);
            // A handle, which the collector follows and a state file refuses
            // as any class handle (so a saved state never holds a waiter's
            // counts; a checkpoint keeps the heap whole).
            self.m
                .variables
                .get_mut(&object_id)
                .expect("an event object")
                .value_kind = celox_design::ValueKind::ObjRef;
            self.assign(&sv::ir::LValue::Ident(object.clone()), handle)?;
            let (_, count) = self.m.temp("event_count", 32, false, false);
            let value = moves.then(|| {
                let width = field.width as usize;
                let (_, value) = self
                    .m
                    .temp("event_value", width, field.signed, field.four_state);
                value
            });
            let load = sv::ir::Expr::Call {
                callee: sv::ir::Callee::Object(Box::new(ObjectOp::Load(*field))),
                args: vec![Some(sv::ir::Expr::Ident(object.clone()))],
                arg_order: Vec::new(),
            };
            if let Some(value) = &value {
                self.assign(&sv::ir::LValue::Ident(value.clone()), &load)?;
            }
            samples.push((*field, handle.clone(), *edge, object, count, value));
        }
        for (field, _, edge, object, count, _) in &samples {
            let current = self.edge_count(object, field, *edge)?;
            let id = self.m.id(count).expect("a count");
            self.store(id, SIROffset::Static(0), 32, current);
        }
        terms.sort_unstable();
        terms.dedup();
        let site = self.wait_site(terms, Vec::new());
        let point = self.suspend_at(site, false)?;
        let mut occurred: Option<RegisterId> = None;
        for (field, handle, edge, object, count, value) in &samples {
            let id = self.m.id(count).expect("a count");
            let before = self.b.alloc_bit(32, false);
            self.b.emit(SIRInstruction::Load(
                before,
                stable(id),
                SIROffset::Static(0),
                32,
            ));
            let now = self.edge_count(object, field, *edge)?;
            let advanced = self.b.alloc_bit(1, false);
            self.b
                .emit(SIRInstruction::Binary(advanced, now, BinaryOp::Ne, before));
            let item = match value {
                // Another object now: the change of the value decides.
                Some(value) => {
                    let (_, current) = self.m.temp(
                        "event_value",
                        field.width as usize,
                        field.signed,
                        field.four_state,
                    );
                    let load = sv::ir::Expr::Call {
                        callee: sv::ir::Callee::Object(Box::new(ObjectOp::Load(*field))),
                        args: vec![Some(handle.clone())],
                        arg_order: Vec::new(),
                    };
                    self.assign(&sv::ir::LValue::Ident(current.clone()), &load)?;
                    let switched = sv::ir::Expr::Binary {
                        left: Box::new(handle.clone()),
                        op: sv::ir::BinaryOp::Ne,
                        right: Box::new(sv::ir::Expr::Ident(object.clone())),
                    };
                    let (switched, _) = self.eval_truth(&switched)?;
                    let (changed, _) =
                        self.eval_truth(&super::calls::edge_occurred_expr(value, &current, *edge))?;
                    let by_switch = self.b.alloc_bit(1, false);
                    self.b.emit(SIRInstruction::Binary(
                        by_switch,
                        switched,
                        BinaryOp::And,
                        changed,
                    ));
                    let either = self.b.alloc_bit(1, false);
                    self.b.emit(SIRInstruction::Binary(
                        either,
                        advanced,
                        BinaryOp::Or,
                        by_switch,
                    ));
                    either
                }
                None => advanced,
            };
            occurred = Some(match occurred {
                Some(any) => {
                    let either = self.b.alloc_bit(1, false);
                    self.b
                        .emit(SIRInstruction::Binary(either, any, BinaryOp::Or, item));
                    either
                }
                None => item,
            });
        }
        let occurred = occurred.expect("an item");
        self.wake_at(point, occurred)?;
        Ok(true)
    }

    /// The count of `edge` occurrences of `field` in the object variable
    /// `object` holds, a 32-bit register: changes for any edge, rises,
    /// falls, or both.
    fn edge_count(
        &mut self,
        object: &str,
        field: &sv::ir::ObjectField,
        edge: sv::ir::EventEdge,
    ) -> Result<RegisterId, sv::AnalyzerError> {
        let handle = self.handle(&sv::ir::Expr::Ident(object.to_string()))?;
        let offset = self.constant64(u64::from(field.offset));
        let count = |this: &mut Self, kind: u64| {
            let kind = this.constant64(kind);
            let raw = this.heap_call(ops::OBJECT_EDGE_COUNT, &[handle, offset, kind]);
            let narrow = this.b.alloc_bit(32, false);
            this.b.emit(SIRInstruction::Slice(narrow, raw, 0, 32));
            narrow
        };
        Ok(match edge {
            sv::ir::EventEdge::Any => count(self, 0),
            sv::ir::EventEdge::Pos => count(self, 1),
            sv::ir::EventEdge::Neg => count(self, 2),
            sv::ir::EventEdge::Both => {
                let rises = count(self, 1);
                let falls = count(self, 2);
                let both = self.b.alloc_bit(32, false);
                self.b
                    .emit(SIRInstruction::Binary(both, rises, BinaryOp::Add, falls));
                both
            }
        })
    }

    /// Count a store of every watched property: `randomize` writes the
    /// random properties of objects whose classes it finds as it runs.
    fn properties_randomized(&mut self) {
        let mut ids: Vec<_> = self
            .m
            .name_to_id
            .iter()
            .filter(|(name, _)| {
                name.strip_prefix(PROPERTY_CHANGES)
                    .is_some_and(|rest| rest.starts_with('@'))
            })
            .map(|(_, &id)| id)
            .collect();
        ids.sort_unstable();
        for id in ids {
            self.count_property_change(id);
        }
    }

    fn count_property_change(&mut self, id: SourceVarId) {
        let count = self.b.alloc_bit(PROPERTY_CHANGES_WIDTH, false);
        self.b.emit(SIRInstruction::Load(
            count,
            stable(id),
            SIROffset::Static(0),
            PROPERTY_CHANGES_WIDTH,
        ));
        let one = self.b.alloc_bit(PROPERTY_CHANGES_WIDTH, false);
        self.b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
        let next = self.b.alloc_bit(PROPERTY_CHANGES_WIDTH, false);
        self.b
            .emit(SIRInstruction::Binary(next, count, BinaryOp::Add, one));
        self.store(id, SIROffset::Static(0), PROPERTY_CHANGES_WIDTH, next);
    }

    /// `expr` converted for property `field` when either is real: a real
    /// value rounds to the nearest integer of an integral property's width,
    /// and a real property holds the real value of `expr` (IEEE 1800-2023
    /// 6.12.1). `None` when neither is real.
    fn real_field_value(
        &mut self,
        expr: &sv::ir::Expr,
        field: &sv::ir::ObjectField,
    ) -> Result<Option<RegisterId>, sv::AnalyzerError> {
        let from = self.m.expr_float_precision(expr);
        let to = floating::field_precision(field);
        if from.is_none() && to.is_none() {
            return Ok(None);
        }
        let signed = self.expr_signed(expr);
        let mut arena = SLTNodeArena::new();
        let node = self.expr_slt(expr, None, &mut arena)?;
        let node = floating::convert(
            &mut arena,
            node,
            from,
            to,
            field.width as usize,
            if from.is_none() { signed } else { field.signed },
            true,
        )
        .ok_or_else(|| unsupported("real property conversion"))?;
        self.lower_slt(&arena, node).map(Some)
    }

    /// The value of object operation `op`, as a temporary.
    pub(super) fn object_call(
        &mut self,
        op: &ObjectOp,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let mut args = args
            .iter()
            .map(|arg| {
                arg.clone()
                    .ok_or_else(|| unsupported(format!("omitted argument of {op}")))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if args.iter().any(|arg| self.m.calls(arg)) {
            for (index, arg) in args.iter_mut().enumerate() {
                // A record is built from a flat structure value, not a handle.
                if let ObjectOp::Box(record) = op {
                    // The value extends by its own signedness (IEEE
                    // 1800-2023 10.7).
                    let signed = self.expr_signed(arg);
                    *arg = self.heap_operand(arg, Some((record.width(), signed)), None)?;
                    continue;
                }
                let (width, signed) = if index == 0 {
                    (32, false)
                } else {
                    match op {
                        // A stored value extends by its own signedness
                        // (IEEE 1800-2023 10.7).
                        ObjectOp::Store(field) => (field.width as usize, self.expr_signed(arg)),
                        ObjectOp::Randomize(_) if index == 1 => (1, false),
                        ObjectOp::ConstraintModeSet(_)
                        | ObjectOp::ModeSet(_)
                        | ObjectOp::ModeSelect(..) => (1, false),
                        ObjectOp::Seed => (32, false),
                        _ => (64, false),
                    }
                };
                // A real value, or any value of a real property, keeps its
                // type and width until the store converts it.
                let real = index == 1
                    && matches!(op, ObjectOp::Store(field)
                        if field.real || self.m.expr_float_precision(arg).is_some());
                *arg = self.heap_operand(
                    arg,
                    (!real).then_some((width, signed)),
                    (index == 0).then_some(celox_design::ValueKind::ObjRef),
                )?;
            }
        }
        let result = match op {
            ObjectOp::Randomize(program) => {
                let object = self.handle(&args[0])?;
                let check = self.integer_of_width(&args[1], 1, false)?;
                let bytes = serde_json::to_vec(program).expect("constraint program serialization");
                let program = self.frozen_bytes(&bytes);
                let result = if args.len() == 2 {
                    self.heap_call(ops::OBJECT_RANDOMIZE, &[object, program, check])
                } else {
                    let mut inputs = self.heap_call(ops::NEW, &[]);
                    let one = self.constant64(1);
                    inputs = self.heap_call(ops::APPEND_BYTES, &[inputs, check, one]);
                    for value in &args[2..] {
                        let value = self.integer_of_width(value, 64, false)?;
                        let eight = self.constant64(8);
                        inputs = self.heap_call(ops::APPEND_BYTES, &[inputs, value, eight]);
                    }
                    self.heap_call(ops::OBJECT_RANDOMIZE_INPUTS, &[object, program, inputs])
                };
                self.properties_randomized();
                result
            }
            ObjectOp::ConstraintModeGet(index) => {
                let object = self.handle(&args[0])?;
                self.heap_call(
                    ops::encode(ops::OBJECT_CONSTRAINT_MODE_GET, u64::from(*index)),
                    &[object],
                )
            }
            ObjectOp::ConstraintModeSet(index) => {
                let object = self.handle(&args[0])?;
                let enabled = self.integer_of_width(&args[1], 1, false)?;
                self.heap_call(
                    ops::encode(
                        ops::OBJECT_CONSTRAINT_MODE_SET,
                        u64::from(index.unwrap_or(u32::MAX)),
                    ),
                    &[object, enabled],
                )
            }
            ObjectOp::Container { .. } => unreachable!("handled by container_call"),
            ObjectOp::Locator(_) => unreachable!("handled by locator"),
            ObjectOp::Pattern(_) => unreachable!("handled by aggregate_pattern"),
            ObjectOp::Concat(_) => unreachable!("handled by container_concat"),
            // Each declaration of an event creates a synchronization object
            // (IEEE 1800-2023 15.5).
            ObjectOp::NewEvent => self.heap_call(ops::EVENT_NEW, &[]),
            ObjectOp::Stream(_) => unreachable!("handled by stream_call"),
            ObjectOp::New {
                class,
                size,
                handles,
            } => {
                let size = self.constant64(u64::from(*size));
                let object =
                    self.heap_call(ops::encode(ops::OBJECT_NEW, u64::from(*class)), &[size]);
                // Object seeding consumes the creating process's RNG once,
                // before the constructor body (IEEE 1800-2023 18.14.3).
                let seed = self.urandom(None)?;
                let seed = self.integer_of_width(&seed, 32, false)?;
                self.heap_call(ops::OBJECT_SEED, &[object, seed]);
                for &(offset, kind) in handles {
                    let offset = self.constant64(u64::from(offset));
                    let kind = self.constant64(match kind {
                        HandleProperty::String => 0,
                        HandleProperty::Object => 1,
                    });
                    self.heap_call(ops::OBJECT_HANDLE, &[object, offset, kind]);
                }
                object
            }
            ObjectOp::Load(field) if field.four_state && self.m.four_state => {
                let object = self.handle(&args[0])?;
                let result = self.load_field(object, *field);
                let width = field.width as usize;
                let (temp, name) = self.m.temp("object", width, field.signed, true);
                self.store(temp, SIROffset::Static(0), width, result);
                return Ok(sv::ir::Expr::Ident(name));
            }
            ObjectOp::Load(field) => {
                let object = self.handle(&args[0])?;
                self.load_field(object, *field)
            }
            ObjectOp::Unbox(record) => {
                let object = self.handle(&args[0])?;
                let parts = record
                    .fields
                    .iter()
                    .map(|field| self.load_field(object, *field))
                    .collect::<Vec<_>>();
                let width = record.width();
                let four_state = self.m.four_state && record.fields.iter().any(|f| f.four_state);
                let value = if four_state {
                    self.b.alloc_logic(width)
                } else {
                    self.b.alloc_bit(width, false)
                };
                self.b.emit(SIRInstruction::Concat(value, parts));
                let (temp, name) = self.m.temp("record", width, false, four_state);
                self.store(temp, SIROffset::Static(0), width, value);
                return Ok(sv::ir::Expr::Ident(name));
            }
            ObjectOp::Box(record) => {
                // Evaluate the value first: its calls may end the block, so
                // the record's handle is only used after them.
                let expr = self.hoist(&args[0])?;
                let width = record.width();
                let signed = self.expr_signed(&expr);
                let mut arena = SLTNodeArena::new();
                let node = self.expr_slt(&expr, Some((width, signed)), &mut arena)?;
                let node =
                    coerce_node_width(&mut arena, node, Some(width), signed).map_err(slt_error)?;
                let value = self.lower_slt(&arena, node)?;
                // A record is not seeded: it has no random state, and seeding
                // would draw from the creating process's RNG.
                let size = self.constant64(u64::from(record.size));
                let object = self.heap_call(
                    ops::encode(ops::OBJECT_NEW, u64::from(sv::ir::Record::CLASS)),
                    &[size],
                );
                for field in &record.fields {
                    if let Some(kind) = field.handle {
                        let offset = self.constant64(u64::from(field.offset));
                        let kind = self.constant64(match kind {
                            HandleProperty::String => 0,
                            HandleProperty::Object => 1,
                        });
                        self.heap_call(ops::OBJECT_HANDLE, &[object, offset, kind]);
                    }
                }
                let mut lsb = width;
                for field in &record.fields {
                    lsb -= field.width as usize;
                    let part = if matches!(self.b.register(&value), RegisterType::Logic { .. }) {
                        self.b.alloc_logic(field.width as usize)
                    } else {
                        self.b.alloc_bit(field.width as usize, false)
                    };
                    self.b.emit(SIRInstruction::Slice(
                        part,
                        value,
                        lsb,
                        field.width as usize,
                    ));
                    self.store_field(object, *field, part);
                }
                // An equal value keeps the handle it already has, so that
                // storing it again leaves a container unchanged.
                self.heap_call(ops::OBJECT_FINISH_RECORD, &[object])
            }
            ObjectOp::Store(field) if field.four_state && self.m.four_state || field.width > 64 => {
                let object = self.handle(&args[0])?;
                let expr = self.hoist(&args[1])?;
                let value = match self.real_field_value(&expr, field)? {
                    Some(value) => value,
                    None => {
                        let signed = self.expr_signed(&expr);
                        self.eval(&expr, Some((field.width as usize, signed)))?
                    }
                };
                let counted = self.edges_begin(object, field);
                self.store_field(object, *field, value);
                self.edges_end(object, field, counted);
                self.property_stored(field);
                self.constant64(0)
            }
            ObjectOp::Store(field) => {
                let object = self.handle(&args[0])?;
                let place =
                    self.constant64(u64::from(field.offset) | u64::from(field.bytes()) << 32);
                let expr = self.hoist(&args[1])?;
                let value = match self.real_field_value(&expr, field)? {
                    Some(value) => {
                        let operand = self.b.alloc_bit(64, false);
                        self.b
                            .emit(SIRInstruction::Unary(operand, UnaryOp::Ident, value));
                        operand
                    }
                    None => {
                        let signed = self.expr_signed(&expr);
                        self.integer_of_width(&expr, field.width as usize, signed)?
                    }
                };
                let counted = self.edges_begin(object, field);
                let result = self.heap_call(ops::OBJECT_STORE, &[object, place, value]);
                self.edges_end(object, field, counted);
                self.property_stored(field);
                result
            }
            ObjectOp::RandomCyclic { offset, count } => {
                let object = self.handle(&args[0])?;
                let count = self.constant64(u64::from(*count));
                self.heap_call(
                    ops::encode(ops::OBJECT_RANDOM_CYCLIC, u64::from(*offset)),
                    &[object, count],
                )
            }
            ObjectOp::Random
            | ObjectOp::Check(_)
            | ObjectOp::ModeGet(..)
            | ObjectOp::ModePush
            | ObjectOp::ModePop
            | ObjectOp::ModeSelected(_) => {
                let object = self.handle(&args[0])?;
                self.heap_call(
                    match op {
                        ObjectOp::Check(class) => ops::encode(ops::OBJECT_CHECK, u64::from(*class)),
                        ObjectOp::ModeGet(offset, random) => ops::encode(
                            ops::OBJECT_MODE_GET,
                            u64::from(*offset) | (u64::from(*random) << 32),
                        ),
                        ObjectOp::ModePush => ops::OBJECT_MODE_PUSH,
                        ObjectOp::ModePop => ops::OBJECT_MODE_POP,
                        ObjectOp::ModeSelected(offset) => {
                            ops::encode(ops::OBJECT_MODE_SELECTED, u64::from(*offset))
                        }
                        _ => ops::OBJECT_RANDOM,
                    },
                    &[object],
                )
            }
            ObjectOp::ModeSet(offset) | ObjectOp::ModeSelect(offset, _) => {
                let object = self.handle(&args[0])?;
                let enabled = self.integer_of_width(&args[1], 1, false)?;
                self.heap_call(
                    match op {
                        ObjectOp::ModeSelect(_, random) => ops::encode(
                            ops::OBJECT_MODE_SELECT,
                            u64::from(*offset) | (u64::from(*random) << 32),
                        ),
                        _ => ops::encode(ops::OBJECT_MODE_SET, u64::from(*offset)),
                    },
                    &[object, enabled],
                )
            }
            ObjectOp::Seed => {
                let object = self.handle(&args[0])?;
                let seed = self.integer_of_width(&args[1], 32, false)?;
                self.heap_call(ops::OBJECT_SEED, &[object, seed])
            }
            ObjectOp::ClassOf => {
                let object = self.handle(&args[0])?;
                self.heap_call(ops::OBJECT_CLASS, &[object])
            }
            ObjectOp::RegisterProgram(class, program) => {
                let class = self.constant64(u64::from(*class));
                let bytes = serde_json::to_vec(program).expect("constraint program serialization");
                let program = self.frozen_bytes(&bytes);
                self.heap_call(ops::OBJECT_REGISTER_PROGRAM, &[class, program])
            }
            ObjectOp::ProgramRegistered(class) => {
                let class = self.constant64(u64::from(*class));
                self.heap_call(ops::OBJECT_PROGRAM_REGISTERED, &[class])
            }
            ObjectOp::VisitBegin => {
                let object = self.handle(&args[0])?;
                self.heap_call(ops::OBJECT_VISIT_BEGIN, &[object])
            }
            ObjectOp::Visit => {
                let index = self.integer_of_width(&args[0], 64, false)?;
                self.heap_call(ops::OBJECT_VISIT, &[index])
            }
            ObjectOp::VisitSolved => self.heap_call(ops::OBJECT_VISIT_SOLVED, &[]),
            ObjectOp::VisitExpand => {
                let object = self.handle(&args[0])?;
                self.heap_call(ops::OBJECT_VISIT_EXPAND, &[object])
            }
            ObjectOp::VisitEnd => self.heap_call(ops::OBJECT_VISIT_END, &[]),
            ObjectOp::Copy => {
                let object = self.handle(&args[0])?;
                self.heap_call(ops::OBJECT_COPY, &[object])
            }
        };
        let (width, signed) = op.result_type();
        let value = self.b.alloc_bit(width, signed);
        self.b.emit(SIRInstruction::Slice(value, result, 0, width));
        let (temp, name) = self.m.temp("object", width, signed, false);
        let kind = match op {
            ObjectOp::Container { .. } => unreachable!("handled by container_call"),
            ObjectOp::Locator(_) => unreachable!("handled by locator"),
            ObjectOp::Pattern(_) => unreachable!("handled by aggregate_pattern"),
            ObjectOp::Concat(_) => unreachable!("handled by container_concat"),
            ObjectOp::Stream(_) => unreachable!("handled by stream_call"),
            ObjectOp::New { .. } | ObjectOp::Copy | ObjectOp::Box(_) | ObjectOp::Visit => {
                Some(celox_design::ValueKind::ObjRef)
            }
            ObjectOp::Unbox(_) => unreachable!("lowered to its own temporary"),
            ObjectOp::Load(field) => field.handle.map(|handle| match handle {
                HandleProperty::String => celox_design::ValueKind::StrRef,
                HandleProperty::Object => celox_design::ValueKind::ObjRef,
            }),
            ObjectOp::Randomize(_)
            | ObjectOp::RegisterProgram(..)
            | ObjectOp::ProgramRegistered(_)
            | ObjectOp::VisitBegin
            | ObjectOp::VisitSolved
            | ObjectOp::VisitExpand
            | ObjectOp::VisitEnd
            | ObjectOp::ConstraintModeGet(_)
            | ObjectOp::ConstraintModeSet(_)
            | ObjectOp::Store(_)
            | ObjectOp::ClassOf
            | ObjectOp::Random
            | ObjectOp::RandomCyclic { .. }
            | ObjectOp::Seed
            | ObjectOp::Check(_)
            | ObjectOp::ModeGet(..)
            | ObjectOp::ModePush
            | ObjectOp::ModePop
            | ObjectOp::ModeSelect(..)
            | ObjectOp::ModeSelected(_)
            | ObjectOp::ModeSet(_)
            | ObjectOp::NewEvent => None,
        };
        if let Some(kind) = kind {
            self.m.variables.get_mut(&temp).unwrap().value_kind = kind;
        }
        if let ObjectOp::Load(field) = op {
            self.m.variables.get_mut(&temp).unwrap().float = floating::field_precision(field);
        }
        self.store(temp, SIROffset::Static(0), width, value);
        Ok(sv::ir::Expr::Ident(name))
    }

    /// The value of `field` of the object in 64-bit `object`: a register of
    /// the field's width, 4-state for a 4-state field in 4-state simulation.
    fn load_field(&mut self, object: RegisterId, field: sv::ir::ObjectField) -> RegisterId {
        let opcode = if field.nullable {
            ops::OBJECT_LOAD_NULLABLE
        } else {
            ops::OBJECT_LOAD
        };
        let width = field.width as usize;
        if field.width > 64 {
            let four_state = field.four_state && self.m.four_state;
            let planes = if four_state {
                vec![0, field.bytes()]
            } else {
                vec![0]
            };
            let planes = planes
                .into_iter()
                .map(|plane| self.load_wide_plane(object, opcode, field, plane, four_state))
                .collect::<Vec<_>>();
            if four_state {
                return self.join_wide_planes(planes[0], planes[1], width);
            }
            let value = self.b.alloc_bit(width, field.signed);
            self.b
                .emit(SIRInstruction::Unary(value, UnaryOp::Ident, planes[0]));
            return value;
        }
        if field.four_state && self.m.four_state {
            let [value, mask] = [0, field.bytes()].map(|plane| {
                let offset = self.constant64(u64::from(field.offset + plane));
                let count = self.constant64(u64::from(field.bytes()));
                let stored = self.heap_call(opcode, &[object, offset, count]);
                self.complement(stored, field.width)
            });
            return self.container_join(value, mask, width, true);
        }
        let offset = self.constant64(u64::from(field.offset));
        let count = self.constant64(u64::from(field.bytes()));
        let loaded = self.heap_call(opcode, &[object, offset, count]);
        let value = self.b.alloc_bit(width, field.signed);
        self.b.emit(SIRInstruction::Slice(value, loaded, 0, width));
        value
    }

    /// The value and mask planes of `value` as two-state registers of `width`
    /// bits, moved in whole words rather than bit by bit.
    fn split_wide_planes(&mut self, value: RegisterId, width: usize) -> (RegisterId, RegisterId) {
        let words = width.next_multiple_of(64);
        let padded = self.b.alloc_logic(words);
        if words > width {
            let zero = self.b.alloc_logic(words - width);
            self.b
                .emit(SIRInstruction::Imm(zero, SIRValue::new(BigUint::zero())));
            self.b
                .emit(SIRInstruction::Concat(padded, vec![zero, value]));
        } else {
            self.b
                .emit(SIRInstruction::Unary(padded, UnaryOp::Ident, value));
        }
        let planes = self.b.alloc_bit(2 * words, false);
        self.b
            .emit(SIRInstruction::Unary(planes, UnaryOp::ToPlanes, padded));
        let [value, mask] = [0, words].map(|lsb| {
            let plane = self.b.alloc_bit(width, false);
            self.b
                .emit(SIRInstruction::Slice(plane, planes, lsb, width));
            plane
        });
        (value, mask)
    }

    /// The four-state register of `width` bits whose value and mask planes
    /// are two-state registers `value` and `mask`.
    fn join_wide_planes(
        &mut self,
        value: RegisterId,
        mask: RegisterId,
        width: usize,
    ) -> RegisterId {
        let words = width.next_multiple_of(64);
        let mut parts = Vec::new();
        for plane in [mask, value] {
            if words > width {
                let zero = self.b.alloc_bit(words - width, false);
                self.b
                    .emit(SIRInstruction::Imm(zero, SIRValue::new(BigUint::zero())));
                parts.push(zero);
            }
            parts.push(plane);
        }
        let planes = self.b.alloc_bit(2 * words, false);
        self.b.emit(SIRInstruction::Concat(planes, parts));
        let padded = self.b.alloc_logic(words);
        self.b
            .emit(SIRInstruction::Unary(padded, UnaryOp::FromPlanes, planes));
        let joined = self.b.alloc_logic(width);
        self.b.emit(SIRInstruction::Slice(joined, padded, 0, width));
        joined
    }

    /// The plane at byte `plane` of a `field` wider than 64 bits, read in
    /// words of at most 8 bytes: a register of the field's width.
    fn load_wide_plane(
        &mut self,
        object: RegisterId,
        opcode: u64,
        field: sv::ir::ObjectField,
        plane: u32,
        complemented: bool,
    ) -> RegisterId {
        let mut words = Vec::new();
        for first in (0..field.width).step_by(64) {
            let bits = (field.width - first).min(64);
            let offset = self.constant64(u64::from(field.offset + plane + first / 8));
            let count = self.constant64(u64::from(bits.div_ceil(8)));
            let loaded = self.heap_call(opcode, &[object, offset, count]);
            let loaded = if complemented {
                self.complement(loaded, bits)
            } else {
                loaded
            };
            let word = self.b.alloc_bit(bits as usize, false);
            self.b
                .emit(SIRInstruction::Slice(word, loaded, 0, bits as usize));
            words.push(word);
        }
        // Concatenation lists the most significant word first.
        words.reverse();
        let value = self.b.alloc_bit(field.width as usize, false);
        self.b.emit(SIRInstruction::Concat(value, words));
        value
    }

    /// Store `plane`, a two-state register of `field`'s width, at byte `at` of
    /// the object in 64-bit `object`, in words of at most 8 bytes.
    fn store_wide_plane(
        &mut self,
        object: RegisterId,
        field: sv::ir::ObjectField,
        at: u32,
        plane: RegisterId,
        complemented: bool,
    ) {
        for first in (0..field.width).step_by(64) {
            let bits = (field.width - first).min(64);
            let word = self.b.alloc_bit(bits as usize, false);
            self.b.emit(SIRInstruction::Slice(
                word,
                plane,
                first as usize,
                bits as usize,
            ));
            let wide = self.b.alloc_bit(64, false);
            self.b
                .emit(SIRInstruction::Unary(wide, UnaryOp::Ident, word));
            let wide = if complemented {
                self.complement(wide, bits)
            } else {
                wide
            };
            let place = self.constant64(
                u64::from(field.offset + at + first / 8) | u64::from(bits.div_ceil(8)) << 32,
            );
            self.heap_call(ops::OBJECT_STORE, &[object, place, wide]);
        }
    }

    /// Store register `value` of `field`'s width into `field` of the object in
    /// 64-bit `object`.
    fn store_field(&mut self, object: RegisterId, field: sv::ir::ObjectField, value: RegisterId) {
        if field.width > 64 {
            if field.four_state && self.m.four_state {
                let (value, mask) = self.split_wide_planes(value, field.width as usize);
                self.store_wide_plane(object, field, 0, value, true);
                self.store_wide_plane(object, field, field.bytes(), mask, true);
            } else {
                let known = self.b.alloc_bit(field.width as usize, false);
                self.b
                    .emit(SIRInstruction::Unary(known, UnaryOp::ToTwoState, value));
                self.store_wide_plane(object, field, 0, known, false);
            }
            return;
        }
        let planes = if field.four_state && self.m.four_state {
            let (value, mask) = self.container_split(value);
            vec![
                (0, self.complement(value, field.width)),
                (field.bytes(), self.complement(mask, field.width)),
            ]
        } else {
            let known = self.b.alloc_bit(field.width as usize, false);
            self.b
                .emit(SIRInstruction::Unary(known, UnaryOp::ToTwoState, value));
            let wide = self.b.alloc_bit(64, false);
            self.b
                .emit(SIRInstruction::Unary(wide, UnaryOp::Ident, known));
            vec![(0, wide)]
        };
        for (plane, bits) in planes {
            let place =
                self.constant64(u64::from(field.offset + plane) | u64::from(field.bytes()) << 32);
            self.heap_call(ops::OBJECT_STORE, &[object, place, bits]);
        }
    }

    /// A 4-state property holds the complement of its value and mask planes,
    /// so the zeroed bytes of a new object read as X (IEEE 1800-2023 6.8).
    /// Bits past `width` stay zero, so a default value is all zero bytes.
    fn complement(&mut self, plane: RegisterId, width: u32) -> RegisterId {
        let complement = self.b.alloc_bit(64, false);
        self.b
            .emit(SIRInstruction::Unary(complement, UnaryOp::BitNot, plane));
        let bits = self.constant64(u64::MAX >> (64 - width.min(64)));
        let masked = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::Binary(
            masked,
            complement,
            BinaryOp::And,
            bits,
        ));
        masked
    }
}
