//! Event controls on properties of objects (IEEE 1800-2023 9.4.2): the
//! objects a wait watches, and the samples that tell whether an item
//! occurred.

use super::*;

impl Ff<'_, '_> {
    /// The property and the object handle of event expression `expr` when it
    /// is a property of an object, or a box, whose stores count (see
    /// `ProcModule::change_counter`): the heap counts its occurrences in each object
    /// an event control watches (see `celox_heap::ops::OBJECT_EDGE_WATCH`).
    pub(super) fn watched_property(
        &self,
        expr: &sv::ir::Expr,
    ) -> Option<(sv::ir::ObjectField, sv::ir::Expr)> {
        let sv::ir::Expr::Call {
            callee: sv::ir::Callee::Object(op),
            args,
            ..
        } = expr
        else {
            return None;
        };
        let (ObjectOp::Load(field), [Some(handle)]) = (&**op, args.as_slice()) else {
            return None;
        };
        self.m
            .change_counter(field)
            .map(|_| (*field, handle.clone()))
    }

    /// A variable holding a sample of an event control's item at a wait, of
    /// the item's type; a real one holds its bits (see
    /// [`Self::sampled_value`]). A forked kernel keeps one per activation; a
    /// unit's lowering makes it a slot of its frame.
    pub(super) fn event_sample(
        &mut self,
        purpose: &str,
        width: usize,
        signed: bool,
        four_state: bool,
        float: Option<celox_sir::FloatPrecision>,
    ) -> Result<String, sv::AnalyzerError> {
        let (width, signed, four_state) = match float {
            Some(celox_sir::FloatPrecision::F64) => (64, false, false),
            Some(celox_sir::FloatPrecision::F32) => (32, false, false),
            None => (width, signed, four_state),
        };
        let (id, name) = self.m.temp(purpose, width, signed, four_state);
        if self.activation.forked && self.kernel.is_some() {
            let slot = self.activation.frame_locals.len();
            self.activation.frame_locals.push(id);
            self.with_kernel(|kernel| kernel.local(slot))
                .map_err(kernel_error)?;
        }
        Ok(name)
    }

    /// What an [`Self::event_sample`] of `expr` holds: its value, or the bits
    /// of a real, so that every change of it is one, however small (as an
    /// event expression's variable keeps them).
    pub(super) fn sampled_value(&self, expr: sv::ir::Expr) -> sv::ir::Expr {
        let function = match self.m.expr_float_precision(&expr) {
            None => return expr,
            Some(celox_sir::FloatPrecision::F64) => sv::ir::SystemFunction::Realtobits,
            Some(celox_sir::FloatPrecision::F32) => sv::ir::SystemFunction::Shortrealtobits,
        };
        sv::ir::Expr::Call {
            callee: sv::ir::Callee::System(function),
            args: vec![Some(expr)],
            arg_order: Vec::new(),
        }
    }

    /// An event control with an item that is a property of an object (IEEE
    /// 1800-2023 9.4.2), in a kernel or a unit. Each item is waited on as
    /// what it is:
    ///
    /// - a property: the process registers the object the handle denotes
    ///   with the heap, which counts the property's occurrences in it at
    ///   every store, and samples its count. It waits for a store of the
    ///   property, which bumps the key's change count, or for a change of a
    ///   handle read from module storage. The item occurs when the object's
    ///   count advanced, or, when the handle now denotes another object,
    ///   when the property's value changed by the item's edge from the old
    ///   object's to the new one's;
    /// - any other expression: the process samples its value and waits for
    ///   a change of what it reads (see `level_terms`); the item occurs when
    ///   the value changed by its edge.
    ///
    /// At each resume an item occurs when it did and its `iff` holds
    /// (9.4.2.3). Without an occurrence the process samples every item
    /// again, a property from the object its handle now denotes, and waits
    /// again. Returns whether `items` have such a property.
    pub(in crate::lowering::ff) fn property_event(
        &mut self,
        items: &[sv::ir::EventItem],
    ) -> Result<bool, sv::AnalyzerError> {
        let exprs: Vec<sv::ir::Expr> = items
            .iter()
            .map(|item| self.propagate(&item.expr))
            .collect();
        if !exprs
            .iter()
            .any(|expr| self.watched_property(expr).is_some())
        {
            return Ok(false);
        }
        let mut terms = Vec::new();
        let mut gated = Vec::new();
        let mut watched = Vec::new();
        for (item, expr) in items.iter().zip(exprs) {
            self.check_event_edge(item.edge, &expr)?;
            let float = self.m.expr_float_precision(&expr);
            if let Some((field, handle)) = self.watched_property(&expr) {
                self.check_event_purity(&handle)?;
                let changes = self.m.change_counter(&field).expect("a watched property");
                terms.push((changes, CounterKind::Changes));
                // A handle in the frame or in the process cannot change while
                // the process waits; one in module storage can.
                let handle_terms = self.level_terms(&handle)?;
                let moves = !handle_terms.is_empty();
                terms.extend(
                    handle_terms
                        .into_iter()
                        .map(|id| (id, CounterKind::Changes)),
                );
                let (object_id, object) =
                    self.m
                        .temp("event_object", celox_design::HANDLE_WIDTH, false, false);
                // A handle, which the collector follows and a state file
                // refuses as any class handle (so a saved state never holds
                // a waiter's counts; a checkpoint keeps the heap whole).
                self.m
                    .variables
                    .get_mut(&object_id)
                    .expect("an event object")
                    .value_kind = celox_design::ValueKind::ObjRef;
                if self.activation.forked && self.kernel.is_some() {
                    let slot = self.activation.frame_locals.len();
                    self.activation.frame_locals.push(object_id);
                    self.with_kernel(|kernel| kernel.local(slot))
                        .map_err(kernel_error)?;
                }
                let count = self.event_sample("event_count", 32, false, false, None)?;
                // The value and, at each resume, the value the handle's
                // object holds now; every sample is made before the
                // process first suspends.
                let value = if moves {
                    let mut sample = || {
                        self.event_sample(
                            "event_value",
                            field.width as usize,
                            field.signed,
                            field.four_state,
                            float,
                        )
                    };
                    Some((sample()?, sample()?))
                } else {
                    None
                };
                // An occurrence happens inside a store, where no `iff` can be
                // evaluated: only a condition that cannot change while the
                // process waits is judged, by its value at the start.
                let gate = match &item.iff {
                    Some(iff) => {
                        let iff = self.propagate(iff);
                        if !self.frozen_condition(&iff) {
                            return Err(unsupported(
                                "an `iff` that the process does not hold alone on an event \
                                 control on a class property",
                            ));
                        }
                        let sample = self.event_sample("event_gate", 1, false, false, None)?;
                        Some((Box::new(super::super::calls::iff_truth(iff)), sample))
                    }
                    None => None,
                };
                watched.push(Watched::Property {
                    field,
                    handle: Box::new(handle),
                    edge: item.edge,
                    object,
                    count,
                    value,
                    gate,
                });
            } else {
                // Any other item is counted where its variable is written,
                // as the edges of a kernel's items are (IEEE 1800-2023
                // 9.4.2): a pulse within one step is an occurrence. Only a
                // variable has such counts.
                let id = match &expr {
                    sv::ir::Expr::Ident(name) => self.m.id(name).filter(|&id| {
                        !self.m.is_hidden(id) && self.m.var(id).array_dims.is_empty()
                    }),
                    _ => None,
                }
                .ok_or_else(|| {
                    unsupported("an event control on a class property and an expression")
                })?;
                let (signal, held) = match &item.iff {
                    None => {
                        let held = self.m.held_gate();
                        (held, Some(held))
                    }
                    Some(iff) => {
                        let iff = self.propagate(iff);
                        let signal = match &iff {
                            sv::ir::Expr::Ident(name) => self.m.id(name).filter(|&id| {
                                let variable = self.m.var(id);
                                !self.m.is_hidden(id)
                                    && variable.width == 1
                                    && variable.float.is_none()
                                    && variable.array_dims.is_empty()
                            }),
                            _ => None,
                        }
                        .ok_or_else(|| {
                            unsupported(
                                "an `iff` other than a one-bit variable on an event control \
                                 with a class property",
                            )
                        })?;
                        (signal, None)
                    }
                };
                let kinds: &[CounterKind] = match item.edge {
                    sv::ir::EventEdge::Any => &[CounterKind::Changes],
                    sv::ir::EventEdge::Pos => &[CounterKind::Rises],
                    sv::ir::EventEdge::Neg => &[CounterKind::Falls],
                    sv::ir::EventEdge::Both => &[CounterKind::Rises, CounterKind::Falls],
                };
                let mut counts = Vec::new();
                // The counts and the held gate are the module's, which every
                // process's writes bump and judge; a unit stages them.
                if let Some(held) = held {
                    self.stage(held);
                }
                for &kind in kinds {
                    let counters = self.gate_counters();
                    self.stage(counters.gated);
                    self.stage(counters.all);
                    gated.push((
                        (id, kind),
                        WaitGate {
                            signal,
                            at_write: true,
                            counters: Some(counters),
                        },
                    ));
                    let sample = self.event_sample(
                        "event_count",
                        celox_design::EVENT_COUNTER_WIDTH,
                        false,
                        false,
                        None,
                    )?;
                    counts.push((counters.gated, sample));
                }
                watched.push(Watched::Counted { counts, held });
            }
        }
        self.sample_watched(&watched)?;
        terms.sort_unstable();
        terms.dedup();
        let mut gates: Vec<_> = terms.iter().map(|_| None).collect();
        for (term, gate) in gated {
            terms.push(term);
            gates.push(Some(gate));
        }
        if gates.iter().all(Option::is_none) {
            gates.clear();
        }
        let site = self.wait_site(terms, gates);
        let point = self.suspend_at(site, false)?;
        let mut occurred: Option<RegisterId> = None;
        for (item, watched) in items.iter().zip(&watched) {
            let happened = self.watched_occurred(watched, item.edge)?;
            occurred = Some(match occurred {
                Some(any) => {
                    let either = self.b.alloc_bit(1, false);
                    self.b
                        .emit(SIRInstruction::Binary(either, any, BinaryOp::Or, happened));
                    either
                }
                None => happened,
            });
        }
        let occurred = occurred.expect("an item");
        // Without an occurrence, sample again before waiting again.
        let again = self.b.new_block();
        let join = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: occurred,
            true_block: (join, Vec::new()),
            false_block: (again, Vec::new()),
        });
        self.b.switch_to_block(again);
        self.sample_watched(&watched)?;
        self.b.seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.b.switch_to_block(join);
        self.wake_at(point, occurred)?;
        Ok(true)
    }

    /// Whether `watched` occurred since it was sampled, by `edge` (see
    /// [`Self::property_event`]): a one-bit register.
    pub(super) fn watched_occurred(
        &mut self,
        watched: &Watched,
        edge: sv::ir::EventEdge,
    ) -> Result<RegisterId, sv::AnalyzerError> {
        match watched {
            Watched::Property {
                field,
                handle,
                object,
                count,
                value,
                gate,
                ..
            } => {
                let id = self.m.id(count).expect("a count");
                let before = self.b.alloc_bit(32, false);
                self.b.emit(SIRInstruction::Load(
                    before,
                    stable(id),
                    SIROffset::Static(0),
                    32,
                ));
                let now = self.edge_count(object, field, edge)?;
                let advanced = self.b.alloc_bit(1, false);
                self.b
                    .emit(SIRInstruction::Binary(advanced, now, BinaryOp::Ne, before));
                // The item's `iff`, sampled at the start, gates every
                // occurrence of it: by the object's count and by a switch.
                let gated = |this: &mut Self, occurred: RegisterId| match gate {
                    Some((_, sample)) => {
                        let id = this.m.id(sample).expect("a gate sample");
                        let holds = this.b.alloc_bit(1, false);
                        this.b.emit(SIRInstruction::Load(
                            holds,
                            stable(id),
                            SIROffset::Static(0),
                            1,
                        ));
                        let both = this.b.alloc_bit(1, false);
                        this.b
                            .emit(SIRInstruction::Binary(both, occurred, BinaryOp::And, holds));
                        both
                    }
                    None => occurred,
                };
                let Some((value, current)) = value else {
                    return Ok(gated(self, advanced));
                };
                // Another object now: the change of the value decides.
                let load = self.sampled_value(sv::ir::Expr::Call {
                    callee: sv::ir::Callee::Object(Box::new(ObjectOp::Load(*field))),
                    args: vec![Some((**handle).clone())],
                    arg_order: Vec::new(),
                });
                self.assign(&sv::ir::LValue::Ident(current.clone()), &load)?;
                let switched = sv::ir::Expr::Binary {
                    left: handle.clone(),
                    op: sv::ir::BinaryOp::Ne,
                    right: Box::new(sv::ir::Expr::Ident(object.clone())),
                };
                let (switched, _) = self.eval_truth(&switched)?;
                let (changed, _) = self.eval_truth(&super::super::calls::edge_occurred_expr(
                    value, current, edge,
                ))?;
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
                Ok(gated(self, either))
            }
            Watched::Counted { counts, .. } => {
                let mut any: Option<RegisterId> = None;
                for (counter, sample) in counts {
                    let width = celox_design::EVENT_COUNTER_WIDTH;
                    let now = self.b.alloc_bit(width, false);
                    self.b.emit(SIRInstruction::Load(
                        now,
                        stable(*counter),
                        SIROffset::Static(0),
                        width,
                    ));
                    let id = self.m.id(sample).expect("a count sample");
                    let before = self.b.alloc_bit(width, false);
                    self.b.emit(SIRInstruction::Load(
                        before,
                        stable(id),
                        SIROffset::Static(0),
                        width,
                    ));
                    let advanced = self.b.alloc_bit(1, false);
                    self.b
                        .emit(SIRInstruction::Binary(advanced, now, BinaryOp::Ne, before));
                    any = Some(match any {
                        Some(either) => {
                            let both = self.b.alloc_bit(1, false);
                            self.b.emit(SIRInstruction::Binary(
                                both,
                                either,
                                BinaryOp::Or,
                                advanced,
                            ));
                            both
                        }
                        None => advanced,
                    });
                }
                Ok(any.expect("a count"))
            }
        }
    }

    /// Whether `condition` reads only what the waiting process alone writes,
    /// and constants, and calls nothing: the frame of a unit, or a process's
    /// own variables, of a kernel that does not fork (the children of a fork
    /// share its variables). Its value at the start of a wait is its value
    /// at every occurrence.
    pub(super) fn frozen_condition(&self, condition: &sv::ir::Expr) -> bool {
        if self.kernel.is_none() {
            return self.frame_only(condition);
        }
        let mut calls = Vec::new();
        collect_calls(condition, &mut calls);
        let mut names = HashSet::default();
        super::super::super::procedural::expr_idents(condition, &mut names);
        calls.is_empty()
            && names.iter().all(|name| {
                self.m.constants.contains_key(name)
                    || !self.activation.forked
                        && self.m.id(name).is_some_and(|id| {
                            self.m.is_hidden(id) && !self.is_static_task_formal(id)
                        })
            })
    }

    /// Sample each of `watched` (see [`Self::property_event`]).
    pub(super) fn sample_watched(&mut self, watched: &[Watched]) -> Result<(), sv::AnalyzerError> {
        for watched in watched {
            match watched {
                Watched::Property {
                    field,
                    handle,
                    edge,
                    object,
                    count,
                    value,
                    gate,
                } => {
                    self.assign(&sv::ir::LValue::Ident(object.clone()), handle)?;
                    // The property is read as the item reads it, so a null
                    // handle faults here as there (IEEE 1800-2023 8.4).
                    self.eval(
                        &sv::ir::Expr::Call {
                            callee: sv::ir::Callee::Object(Box::new(ObjectOp::Load(*field))),
                            args: vec![Some(sv::ir::Expr::Ident(object.clone()))],
                            arg_order: Vec::new(),
                        },
                        None,
                    )?;
                    if let Some((iff, sample)) = gate {
                        self.assign(&sv::ir::LValue::Ident(sample.clone()), iff)?;
                    }
                    let register = self.handle(&sv::ir::Expr::Ident(object.clone()))?;
                    let place =
                        self.constant64(u64::from(field.offset) | u64::from(field.bytes()) << 32);
                    let four_state =
                        self.constant64(u64::from(field.four_state && self.m.four_state));
                    self.heap_call(ops::OBJECT_EDGE_WATCH, &[register, place, four_state]);
                    let current = self.edge_count(object, field, *edge)?;
                    let id = self.m.id(count).expect("a count");
                    self.store(id, SIROffset::Static(0), 32, current);
                    if let Some((value, _)) = value {
                        let load = self.sampled_value(sv::ir::Expr::Call {
                            callee: sv::ir::Callee::Object(Box::new(ObjectOp::Load(*field))),
                            args: vec![Some(sv::ir::Expr::Ident(object.clone()))],
                            arg_order: Vec::new(),
                        });
                        self.assign(&sv::ir::LValue::Ident(value.clone()), &load)?;
                    }
                }
                Watched::Counted { counts, held } => {
                    if let Some(held) = held {
                        let one = self.b.alloc_bit(1, false);
                        self.b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
                        self.store(*held, SIROffset::Static(0), 1, one);
                    }
                    for (counter, sample) in counts {
                        let width = celox_design::EVENT_COUNTER_WIDTH;
                        let now = self.b.alloc_bit(width, false);
                        self.b.emit(SIRInstruction::Load(
                            now,
                            stable(*counter),
                            SIROffset::Static(0),
                            width,
                        ));
                        let id = self.m.id(sample).expect("a count sample");
                        self.store(id, SIROffset::Static(0), width, now);
                    }
                }
            }
        }
        Ok(())
    }

    /// The count of `edge` occurrences of `field` in the object variable
    /// `object` holds, a 32-bit register: changes for any edge, rises,
    /// falls, or both.
    pub(super) fn edge_count(
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
}

/// An item of an event control with a property of an object (see
/// `Ff::property_event`), and the variables holding its samples.
pub(super) enum Watched {
    /// A property of an object: the object its handle denoted, the count of
    /// the item's edge in it, and, when the handle can denote another object
    /// while the process waits, its value and where the value of the object
    /// the handle denotes at a resume is read.
    Property {
        field: sv::ir::ObjectField,
        handle: Box<sv::ir::Expr>,
        edge: sv::ir::EventEdge,
        object: String,
        count: String,
        value: Option<(String, String)>,
        /// The item's `iff`, which reads only what the process alone
        /// writes, and the sample of its truth taken at the start of the
        /// wait: its value at every occurrence.
        gate: Option<(Box<sv::ir::Expr>, String)>,
    },
    /// A variable, whose occurrences of each of the item's edges the writes
    /// count where they happen, while its gate holds (see
    /// `celox_design::GateCounters`): the gated count of each, and its
    /// sample.
    Counted {
        counts: Vec<(SourceVarId, String)>,
        /// The variable holding 1 that gates an item without `iff`.
        held: Option<SourceVarId>,
    },
}
