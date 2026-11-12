//! The call sites of out-of-line calls: the function a call enters, the
//! places of its `ref` formals, the frame and the arguments of the call, and
//! the suspensions a call passes on to its process.

use super::*;

impl Ff<'_, '_> {
    /// The function a call of `name` enters, when the call goes out of line,
    /// with its `ref` formals bound to `places`.
    pub(in crate::lowering::ff) fn out_of_line_callee(
        &mut self,
        name: &str,
        subroutine: &sv::ir::Subroutine,
        args: &[Option<sv::ir::Expr>],
        places: Option<&[Place]>,
    ) -> Result<Option<usize>, sv::AnalyzerError> {
        if !self.m.out_of_line_policy(name) {
            return Ok(None);
        }
        if let Some(reason) = self.m.out_of_line_reason(name) {
            self.record_outcome(name, Outcome::Inlined(reason));
            return Ok(None);
        }
        // Code that must not call, such as the probe of an event expression,
        // and a call that inlining folds, unless it is recursive and so
        // cannot be inlined.
        let recursive = self
            .call_stack
            .active_calls
            .iter()
            .any(|active| active == name);
        if self.inline_calls {
            self.record_outcome(
                name,
                Outcome::Inlined(Reason::Other("event expression".to_string())),
            );
            return Ok(None);
        }
        // The action of a deferred assertion of a unit waits in the queue of
        // the process the unit runs in (IEEE 1800-2023 16.4.1): a kernel, or
        // a unit only kernels run; elsewhere the body is inlined, its action
        // in the queue of the process that inlines it.
        if self.kernel.is_none() && self.frame.is_none() && self.m.process_state(name).deferred {
            self.record_outcome(
                name,
                Outcome::Inlined(Reason::Other(
                    "deferred assertion outside a process".to_string(),
                )),
            );
            return Ok(None);
        }
        // A level wait's predicate observes the events whose `triggered` it
        // reads, which the inlined body tells the host (see
        // `ProcessState::observes_events`).
        if self.process_predicate && self.m.process_state(name).observes_events {
            self.record_outcome(
                name,
                Outcome::Inlined(Reason::Other("level wait observes its events".to_string())),
            );
            return Ok(None);
        }
        // A call of a subroutine on a cycle of the call graph, or over the
        // size threshold, goes out of line whatever its arguments: neither
        // folds by inlining.
        if !recursive
            && !self.m.out_of_line_whatever_arguments(name)
            && self.m.constant_arguments(subroutine, args)
        {
            self.record_outcome(
                name,
                Outcome::Inlined(Reason::Other("constant arguments".to_string())),
            );
            return Ok(None);
        }
        let Some(places) = places else {
            self.record_outcome(name, Outcome::Inlined(Reason::RefFormal));
            return Ok(None);
        };
        let key = function_key(name, places);
        let Some(functions) = self.m.out_of_line() else {
            return Ok(None);
        };
        if let Some(reason) = functions.rejected.get(&key).cloned() {
            self.record_outcome(name, Outcome::Inlined(reason));
            return Ok(None);
        }
        let index = match functions.by_name.get(&key).copied() {
            Some(index) => index,
            None => match self.lower_function(name, &key, places) {
                Ok(index) => index,
                Err(reason) => {
                    self.record_outcome(name, Outcome::Inlined(reason));
                    return Ok(None);
                }
            },
        };
        // A unit that can suspend needs a caller that can: a process, or a
        // unit that can itself.
        let functions = self.m.out_of_line().expect("checked above");
        if functions.functions[index].resume.is_some() && !self.can_request() {
            self.record_outcome(name, Outcome::Inlined(Reason::HostRequest));
            return Ok(None);
        }
        // An always_ff process keeps its writes in the working region,
        // which the function, reading stable state, would not see.
        if self.kernel.is_none() && !self.direct {
            let functions = self.m.out_of_line().expect("checked above");
            let consistent = functions.transitive(index).is_some_and(|(reads, writes)| {
                writes.is_empty()
                    && reads.iter().all(|id| {
                        !self.writes.working.contains(id)
                            && !self.m.variables.get(id).is_some_and(SvVariable::is_scratch)
                    })
            });
            if !consistent {
                self.record_outcome(
                    name,
                    Outcome::Inlined(Reason::Other("state of an always_ff process".to_string())),
                );
                return Ok(None);
            }
        }
        // A combinational process gives the hidden variables it stores, the
        // static storage of the subroutines it inlines among them, a private
        // copy each (see `lower_comb_process`), which the function would not
        // see: the call passes and returns every value through the frame, and
        // the function touches no hidden variable.
        if self.kernel.is_none() && self.direct && self.frame.is_none() {
            let storage: Vec<SourceVarId> = subroutine
                .params
                .iter()
                .map(|param| &param.name)
                .chain(&subroutine.return_var)
                .filter_map(|name| self.m.id(name))
                .map(|id| self.m.declared(id))
                .collect();
            let functions = self.m.out_of_line().expect("checked above");
            let function = &functions.functions[index];
            let in_frame = storage.iter().all(|&id| function.slot(id).is_some());
            let touched = functions.transitive(index);
            let private = touched.as_ref().is_none_or(|(reads, writes)| {
                reads.iter().chain(writes).any(|&id| self.m.is_hidden(id))
            });
            if !in_frame || private {
                self.record_outcome(
                    name,
                    Outcome::Inlined(Reason::Other(
                        "static storage in a combinational process".to_string(),
                    )),
                );
                return Ok(None);
            }
            // The process's outputs are the variables its own statements
            // assign: a function may write only the static storage of
            // subroutines, which the process does not own, and none that
            // assembly instruments, which it does in the process's own
            // stores only (comb-observer captures).
            self.m.check_storage_index();
            if touched.is_some_and(|(_, writes)| {
                writes
                    .iter()
                    .any(|id| !self.m.is_static_storage(*id) || self.m.is_observed(*id))
            }) {
                self.record_outcome(
                    name,
                    Outcome::Inlined(Reason::Other(
                        "module state in a combinational process".to_string(),
                    )),
                );
                return Ok(None);
            }
        }
        // The children of a unit's fork are activations of the kernel.
        if self.kernel.is_some() && self.m.process_state(name).forks {
            self.with_kernel(ProcessKernelBuilder::forks_in_call);
        }
        self.record_outcome(name, Outcome::OutOfLine);
        Ok(Some(index))
    }

    /// The places of the `ref` formals of `subroutine` for a call that binds
    /// them as `references` (the formals that name a variable, with it)
    /// and `aliases` (the others), in the order of the formals; `None` when
    /// one has no place an out-of-line call can pass: a formal of unpacked
    /// structure type, or one automatic of the calling unit bound to two
    /// formals, which the call copies.
    pub(in crate::lowering::ff) fn ref_places(
        &self,
        subroutine: &sv::ir::Subroutine,
        references: &[(String, SourceVarId, SourceVarId)],
        aliases: &[super::super::references::Alias],
        actual_names: &HashMap<String, String>,
    ) -> Option<Vec<Place>> {
        let mut places = Vec::new();
        let mut copied = HashSet::default();
        for param in subroutine
            .params
            .iter()
            .filter(|param| param.direction.is_reference())
        {
            if !param.leaves.is_empty() {
                return None;
            }
            if let Some(&(_, _, target)) = references
                .iter()
                .find(|(formal, _, _)| *formal == param.name)
            {
                let name = actual_names.get(&param.name)?;
                // An array in the frame of this activation has no place a
                // call can pass: the call stays inlined, where the formal
                // names the slot.
                if self.frame_array(target, name) {
                    return None;
                }
                if self.frame_variable(target, name) {
                    if !copied.insert(target) {
                        return None;
                    }
                    places.push(Place::Frame {
                        id: target,
                        name: name.clone(),
                    });
                } else {
                    places.push(Place::Variable(target));
                }
                continue;
            }
            let alias = aliases.iter().find(|alias| alias.formal == param.name)?;
            places.push(match &alias.target {
                super::super::references::Target::Property { handle, field } => Place::Property {
                    // Places of properties that no wait watches share a
                    // function whichever property they are.
                    field: sv::ir::ObjectField {
                        key: if self.m.change_counter(field).is_some() {
                            field.key
                        } else {
                            sv::ir::ChangeKey::None
                        },
                        ..*field
                    },
                    handle: handle.clone(),
                },
                super::super::references::Target::Element {
                    element,
                    signed,
                    width,
                    total,
                } => {
                    let sv::ir::LValue::Select { name, msb, lsb, .. } = &**element else {
                        return None;
                    };
                    // The array is the caller's variable, which the callee
                    // names by its own name: an inlined activation's copy of
                    // a local is not the variable the local's name declares.
                    // An array in the frame of this activation has no name a
                    // call can pass: the call stays inlined.
                    let id = self.m.id(name)?;
                    if self.frame_array(id, name) {
                        return None;
                    }
                    let own = self.m.var(id).path.first().filter(|own| {
                        self.m.var(id).path.len() == 1 && *own != name && self.m.id(own) == Some(id)
                    });
                    let array = own.cloned().unwrap_or_else(|| name.clone());
                    // The positions the call fixed are hidden variables; the
                    // static part names them by their order.
                    let mut names = HashSet::default();
                    let read = sv::ir::Expr::Select {
                        expr: Box::new(sv::ir::Expr::Literal("1'b0".into())),
                        msb: msb.clone(),
                        lsb: lsb.clone(),
                        signed: false,
                    };
                    super::super::super::procedural::expr_idents(&read, &mut names);
                    let mut positions: Vec<String> = names
                        .into_iter()
                        .filter(|position| position != name)
                        .filter(|position| {
                            self.m.id(position).is_some_and(|id| self.m.is_hidden(id))
                        })
                        .collect();
                    positions.sort_unstable();
                    let renames: HashMap<String, String> = positions
                        .iter()
                        .enumerate()
                        .map(|(k, position)| (position.clone(), format!("#{k}")))
                        .collect();
                    let mut element = (**element).clone();
                    if let sv::ir::LValue::Select { name, msb, lsb, .. } = &mut element {
                        *name = array;
                        *msb = super::super::rename_const_idents(msb, &renames);
                        *lsb = super::super::rename_const_idents(lsb, &renames);
                    }
                    Place::Element {
                        element: Box::new(element),
                        signed: *signed,
                        width: *width,
                        total: *total,
                        positions,
                    }
                }
            });
        }
        Some(places)
    }

    pub(super) fn record_outcome(&mut self, name: &str, outcome: Outcome) {
        if let Some(functions) = self.m.out_of_line() {
            functions.record(name, outcome);
        }
    }

    /// The delay word `word` of a suspension with status `code` from a call
    /// of the kernel being lowered, as the process's: a wait at function
    /// wait site `k` is a wait at the process's site `own + k`, `own` the
    /// number of the kernel's own wait sites, and a nonblocking assignment
    /// at function NBA site `k` one at its NBA site `own + k`, `own` the
    /// number of its own NBA sites; each number is patched in when the
    /// kernel is finished (see `Sites::wait_offset_patches`). `waits` and
    /// `nbas` say which the callee can suspend for.
    pub(super) fn function_site_word(
        &mut self,
        code: RegisterId,
        mut word: RegisterId,
        waits: bool,
        nbas: bool,
    ) -> RegisterId {
        let width = celox_design::PROCESS_DELAY_WIDTH;
        let shift = |this: &mut Self,
                     statuses: &[celox_design::ProcessStatus],
                     nba: bool,
                     word: RegisterId| {
            let mut matched: Option<RegisterId> = None;
            for status in statuses {
                let value = this.b.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
                this.b
                    .emit(SIRInstruction::Imm(value, SIRValue::new(status.code())));
                let equal = this.b.alloc_bit(1, false);
                this.b
                    .emit(SIRInstruction::Binary(equal, code, BinaryOp::Eq, value));
                matched = Some(match matched {
                    Some(any) => {
                        let either = this.b.alloc_bit(1, false);
                        this.b
                            .emit(SIRInstruction::Binary(either, any, BinaryOp::Or, equal));
                        either
                    }
                    None => equal,
                });
            }
            let matched = matched.expect("a status");
            let zeros = this.b.alloc_bit(width - 1, false);
            this.b.emit(SIRInstruction::Imm(zeros, SIRValue::new(0u8)));
            let factor = this.b.alloc_bit(width, false);
            this.b
                .emit(SIRInstruction::Concat(factor, vec![zeros, matched]));
            let own = this.b.alloc_bit(width, false);
            this.b.emit(SIRInstruction::Imm(own, SIRValue::new(0u8)));
            if nba {
                this.sites.nba_offset_patches.push(own);
            } else {
                this.sites.wait_offset_patches.push(own);
            }
            let offset = this.b.alloc_bit(width, false);
            this.b
                .emit(SIRInstruction::Binary(offset, factor, BinaryOp::Mul, own));
            let translated = this.b.alloc_bit(width, false);
            this.b.emit(SIRInstruction::Binary(
                translated,
                word,
                BinaryOp::Add,
                offset,
            ));
            translated
        };
        if waits {
            word = shift(
                self,
                &[
                    celox_design::ProcessStatus::Wait,
                    celox_design::ProcessStatus::Pending,
                ],
                false,
                word,
            );
        }
        if nbas {
            word = shift(self, &[celox_design::ProcessStatus::Nba], true, word);
        }
        word
    }

    /// A hidden variable of the shape of `id`, which receives a value that
    /// a call leaves in the slot of `id`. A call made in a function unit
    /// thereby keeps it in its own frame, apart from the callee's.
    pub(in crate::lowering::ff) fn call_result_temp(
        &mut self,
        id: SourceVarId,
    ) -> (SourceVarId, String) {
        let variable = self.m.var(id).clone();
        let (copy, name) = self.m.temp(
            "call_result",
            variable.width,
            variable.signed,
            variable.is_4state,
        );
        self.m.variables.insert(
            copy,
            SvVariable {
                path: vec![name.clone()],
                kind: VariableKind::Variable,
                source: None,
                hidden: true,
                force_control: None,
                sampled_from: None,
                history_of: None,
                activation_scratch: true,
                ..variable
            },
        );
        (copy, name)
    }

    /// Call function `index`, the unit of `subroutine`: pass `inputs`, the
    /// values of the formals the caller writes, store each `(formal, temp)`
    /// of `copies` from the formal's slot after the call, and return the
    /// variable that holds the result.
    pub(in crate::lowering::ff) fn call_out_of_line(
        &mut self,
        index: usize,
        subroutine: &sv::ir::Subroutine,
        inputs: Vec<(SourceVarId, Result<RegisterId, String>)>,
        copies: &[(SourceVarId, SourceVarId)],
    ) -> Result<Option<String>, sv::AnalyzerError> {
        let function = self.m.out_of_line().expect("calling out of line").functions[index].clone();
        let mut args = Vec::new();
        for (id, value) in inputs {
            let value = match value {
                Ok(value) => value,
                Err(name) => self.eval(&sv::ir::Expr::Ident(name), None)?,
            };
            match function.slot(self.m.declared(id)) {
                Some(slot) => args.push((slot, value)),
                // A static formal whose value persists: the callee reads
                // the module variable, as an inlined body would.
                None => {
                    let width = self.m.var(id).width;
                    let offset = sv_memory_offset(self.m.var(id), 0, width);
                    self.store(id, offset, width, value);
                }
            }
        }
        let (base, depth) = self.call_frame(index, &subroutine.name, &function)?;
        args.insert(0, (0, base));
        args.insert(1, (1, depth));
        let result = subroutine.return_var.as_ref().and_then(|name| {
            let id = self.m.id(name)?;
            Some((id, function.slot(self.m.declared(id))))
        });
        let mut outputs: Vec<(usize, SourceVarId)> = Vec::new();
        let mut result_name = None;
        if let Some((id, slot)) = result {
            match slot {
                Some(slot) => {
                    let (temp, name) = self.call_result_temp(id);
                    outputs.push((slot, temp));
                    result_name = Some(name);
                }
                None => result_name = subroutine.return_var.clone(),
            }
        }
        for &(formal, temp) in copies {
            let slot = function
                .slot(self.m.declared(formal))
                .ok_or_else(|| unsupported("output formal without a frame slot"))?;
            outputs.push((slot, temp));
        }
        // The calling process's RNG states come back: at the end of the
        // call, or when it suspends the process, which then holds them
        // until it continues the call.
        if let Some((state_slot, _)) = function.random {
            let (state, _) = self.urandom_state();
            outputs.push((state_slot, state));
        }
        if let Some(slot) = function.dist_seed {
            let (seed, _) = self.dist_seed();
            outputs.push((slot, seed));
        }
        args.extend(self.process_args(&function)?);
        // A resumable unit starts its body, and leaves whether it suspended.
        let mut control = Vec::new();
        if let Some(resume) = function.resume {
            let width = celox_frontend_core::process::PROCESS_RESUME_WIDTH;
            let start = self.b.alloc_bit(width, false);
            self.b.emit(SIRInstruction::Imm(start, SIRValue::new(0u8)));
            args.push((resume, start));
            for (offset, purpose, width) in [
                (1, "call_status", celox_design::PROCESS_STATUS_WIDTH),
                (2, "call_request", celox_design::PROCESS_DELAY_WIDTH),
                (3, "call_top", celox_design::PROCESS_CALL_TOP_WIDTH),
            ] {
                let (temp, _) = self.m.temp(purpose, width, false, false);
                outputs.push((resume + offset, temp));
                control.push((temp, width));
            }
        }
        self.emit_call(index, &function, base, args, &outputs);
        if let [status, request, top] = control[..] {
            // A suspended call suspends the caller, which continues it by
            // calling again: the callee's frame keeps its formals and its
            // resume point, and only the process state goes in again.
            let check = self.b.new_block();
            self.jump(check);
            self.b.switch_to_block(check);
            let load = |b: &mut SIRBuilder<Addr>, (id, width): (SourceVarId, usize)| {
                let register = b.alloc_bit(width, false);
                b.emit(SIRInstruction::Load(
                    register,
                    stable(id),
                    SIROffset::Static(0),
                    width,
                ));
                register
            };
            let code = load(&mut self.b, status);
            let zero = self.b.alloc_bit(status.1, false);
            self.b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
            let suspended = self.b.alloc_bit(1, false);
            self.b
                .emit(SIRInstruction::Binary(suspended, code, BinaryOp::Ne, zero));
            let suspend = self.b.new_block();
            let done = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: suspended,
                true_block: (suspend, Vec::new()),
                false_block: (done, Vec::new()),
            });
            self.b.switch_to_block(suspend);
            let code = load(&mut self.b, status);
            let mut word = load(&mut self.b, request);
            let end = load(&mut self.b, top);
            // A unit waits at a function wait site, and makes a nonblocking
            // assignment at a function NBA site, which the process lists
            // after the kernel's own sites.
            let state = self.m.process_state(&subroutine.name);
            if self.kernel.is_some() && (state.waits || state.nbas) {
                word = self.function_site_word(code, word, state.waits, state.nbas);
            }
            if self.kernel.is_some() {
                let call_top = match self.call_top {
                    Some(id) => id,
                    None => {
                        let (id, _) = self.m.temp(
                            "call_top",
                            celox_design::PROCESS_CALL_TOP_WIDTH,
                            false,
                            false,
                        );
                        self.call_top = Some(id);
                        id
                    }
                };
                let point = self
                    .with_kernel(|kernel| kernel.suspend_in_call(code, word, end, call_top))
                    .map_err(kernel_error)?;
                self.sites.call_resumes.push(celox_design::CallResume {
                    point,
                    function: index as u32,
                });
            } else {
                let point = self.suspend_unit(code, word, Some(end))?;
                self.resume_frame()
                    .expect("a unit that suspends is resumable")
                    .calls
                    .push(celox_design::CallResume {
                        point,
                        function: index as u32,
                    });
            }
            let (base, depth) = self.call_frame(index, &subroutine.name, &function)?;
            let mut args = vec![(0, base), (1, depth)];
            args.extend(self.process_args(&function)?);
            self.emit_call(index, &function, base, args, &outputs);
            self.jump(check);
            self.b.switch_to_block(done);
        }
        Ok(result_name)
    }

    /// The values of the calling process's state that a call of `function`
    /// passes (see [`Function::random`]): its RNG states and seed, as an
    /// inlined body would draw on them.
    pub(super) fn process_args(
        &mut self,
        function: &Function,
    ) -> Result<Vec<(usize, RegisterId)>, sv::AnalyzerError> {
        let mut args = Vec::new();
        if let Some((state_slot, seed_slot)) = function.random {
            let (_, name) = self.urandom_state();
            let value = self.eval(&sv::ir::Expr::Ident(name), None)?;
            args.push((state_slot, value));
            let seed = self.process_seed();
            let seed = self.eval(&seed, Some((32, false)))?;
            args.push((seed_slot, seed));
        }
        if let Some(slot) = function.dist_seed {
            let (_, name) = self.dist_seed();
            let value = self.eval(&sv::ir::Expr::Ident(name), None)?;
            args.push((slot, value));
        }
        Ok(args)
    }

    /// Call function `index` in the frame at `base` with `args`, and store
    /// each `(slot, variable)` of `outputs` from the slot when it returns.
    pub(super) fn emit_call(
        &mut self,
        index: usize,
        function: &Function,
        base: RegisterId,
        args: Vec<(usize, RegisterId)>,
        outputs: &[(usize, SourceVarId)],
    ) {
        let width: usize = outputs
            .iter()
            .map(|&(slot, _)| function.slots[slot].width)
            .sum();
        let dst = (!outputs.is_empty()).then(|| {
            if outputs
                .iter()
                .any(|&(slot, _)| function.slots[slot].four_state)
            {
                self.b.alloc_logic(width)
            } else {
                self.b.alloc_bit(width, false)
            }
        });
        self.b.emit(SIRInstruction::Call {
            function: celox_sir::FunctionId(index as u32),
            frame: base,
            args,
            outputs: outputs.iter().map(|&(slot, _)| slot).collect(),
            dst,
            effects: Default::default(),
        });
        if let Some(dst) = dst {
            // The first output is the least significant part.
            let mut offset = 0;
            for &(slot, temp) in outputs {
                let slot_width = function.slots[slot].width;
                let part = if function.slots[slot].four_state && self.m.var(temp).is_4state {
                    self.b.alloc_logic(slot_width)
                } else {
                    self.b.alloc_bit(slot_width, false)
                };
                self.b
                    .emit(SIRInstruction::Slice(part, dst, offset, slot_width));
                let variable = self.m.var(temp);
                let store_offset = sv_memory_offset(variable, 0, variable.width);
                self.store(temp, store_offset, slot_width, part);
                offset += slot_width;
            }
        }
    }

    /// The frame base and call depth of a call of function `index`, and the
    /// checks that the call fits the stack and the depth limit, which raise
    /// a runtime error otherwise. A unit run by the runtime calls with an
    /// empty stack; a function calls in the frame after its own.
    pub(super) fn call_frame(
        &mut self,
        index: usize,
        name: &str,
        callee: &Function,
    ) -> Result<(RegisterId, RegisterId), sv::AnalyzerError> {
        let word = |b: &mut SIRBuilder<Addr>, value: u64| {
            let register = b.alloc_bit(32, false);
            b.emit(SIRInstruction::Imm(register, SIRValue::new(value)));
            register
        };
        let Some(frame) = &self.frame else {
            let base = word(&mut self.b, 0);
            let depth = word(&mut self.b, 1);
            return Ok((base, depth));
        };
        let (own, base_var, depth_var) = (frame.function, frame.base, frame.depth);
        let load = |b: &mut SIRBuilder<Addr>, id| {
            let register = b.alloc_bit(32, false);
            b.emit(SIRInstruction::Load(
                register,
                stable(id),
                SIROffset::Static(0),
                32,
            ));
            register
        };
        let add = |b: &mut SIRBuilder<Addr>, left, right| {
            let register = b.alloc_bit(32, false);
            b.emit(SIRInstruction::Binary(register, left, BinaryOp::Add, right));
            register
        };
        let current = load(&mut self.b, base_var);
        let own_size = word(&mut self.b, 0);
        let base = add(&mut self.b, current, own_size);
        let current_depth = load(&mut self.b, depth_var);
        let one = word(&mut self.b, 1);
        let depth = add(&mut self.b, current_depth, one);
        // A callee still being lowered (a recursive call) has no size yet.
        let size = word(&mut self.b, callee.size as u64);
        let frame = self.frame.as_mut().expect("lowering a function unit");
        frame.size_patches.push((own_size, own));
        if callee.unit.is_none() {
            frame.size_patches.push((size, index));
        }
        let end = add(&mut self.b, base, size);
        let limit = word(&mut self.b, celox_sir::STACK_SIZE as u64);
        let overflow = self.b.alloc_bit(1, false);
        self.b
            .emit(SIRInstruction::Binary(overflow, limit, BinaryOp::LtU, end));
        self.raise_if(overflow, format!("stack overflow in a call of `{name}`"));
        let limit = word(&mut self.b, celox_sir::MAX_CALL_DEPTH as u64);
        let deep = self.b.alloc_bit(1, false);
        self.b
            .emit(SIRInstruction::Binary(deep, limit, BinaryOp::LtU, depth));
        self.raise_if(
            deep,
            format!(
                "call depth exceeds {} in a call of `{name}`",
                celox_sir::MAX_CALL_DEPTH
            ),
        );
        Ok((base, depth))
    }

    /// End the run with a runtime error reporting `message` when `condition`
    /// holds, and continue in a new block otherwise.
    pub(super) fn raise_if(&mut self, condition: RegisterId, message: String) {
        let error = self.b.new_block();
        let next = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: condition,
            true_block: (error, Vec::new()),
            false_block: (next, Vec::new()),
        });
        self.b.switch_to_block(error);
        let code = self.m.runtime_error(message);
        self.b.seal_block(SIRTerminator::Error(code));
        self.b.switch_to_block(next);
    }
}
