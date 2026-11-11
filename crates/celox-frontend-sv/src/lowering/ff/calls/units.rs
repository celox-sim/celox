//! Function units: the lowering of a subroutine body into its unit, and
//! the layout of the unit's frame.

use super::*;

impl Ff<'_, '_> {
    /// Whether variable `id`, named `name`, is an automatic of the unit
    /// being lowered that a call copies into a callee's frame and back (see
    /// [`Place::Frame`]): one that is not an unpacked array, which is a
    /// module variable that a callee names, or a slot (see
    /// [`Self::frame_array`]).
    pub(super) fn frame_variable(&self, id: SourceVarId, name: &str) -> bool {
        self.m.var(id).array_dims.is_empty() && self.automatic_of_unit(id, name)
    }

    /// Whether variable `id`, named `name`, is an automatic unpacked array
    /// that is a slot of the frame of the unit being lowered: one of a unit
    /// that can have several live activations.
    pub(super) fn frame_array(&self, id: SourceVarId, name: &str) -> bool {
        !self.m.var(id).array_dims.is_empty()
            && self.frame.as_ref().is_some_and(|frame| !frame.single)
            && self.automatic_of_unit(id, name)
    }

    /// Whether variable `id`, named `name`, is an automatic of the unit
    /// being lowered: a slot of its own, an automatic of a subroutine whose
    /// body it expands, or a variable its lowering created.
    pub(super) fn automatic_of_unit(&self, id: SourceVarId, name: &str) -> bool {
        let Some(frame) = &self.frame else {
            return false;
        };
        let own = self
            .m
            .calls
            .as_deref()
            .and_then(|graph| graph.out_of_line.as_ref())
            .is_some_and(|functions| functions.functions[frame.function].slot(id).is_some());
        own || id.0 >= frame.start.0
            || self.call_stack.active_calls.iter().any(|active| {
                self.m.subroutine(active).is_some_and(|subroutine| {
                    automatic_names(subroutine).any(|local| local == name)
                        && !subroutine
                            .params
                            .iter()
                            .any(|param| param.direction.is_reference() && param.name == name)
                })
            })
    }

    /// Lower subroutine `name` into a new function unit, or say why it
    /// cannot be. A failed lowering leaves nothing behind, so the call is
    /// inlined as if it had never been tried.
    pub(super) fn lower_function(
        &mut self,
        name: &str,
        key: &str,
        places: &[Place],
    ) -> Result<usize, Reason> {
        let subroutine = self
            .m
            .subroutine(name)
            .cloned()
            .ok_or_else(|| Reason::Other(format!("no subroutine `{name}`")))?;
        let checkpoint = self.m.checkpoint();
        let names = self.m.declare_names();
        let lowered = self.lower_function_from(&subroutine, key, places);
        self.m.restore_names(names);
        match lowered {
            Ok(index) => {
                let functions = self.m.out_of_line().expect("lowering out of line");
                functions.patch_sizes();
                Ok(index)
            }
            Err(reason) => {
                self.m.rollback(checkpoint);
                if let Some(functions) = self.m.out_of_line() {
                    functions.rejected.insert(key.to_string(), reason.clone());
                }
                Err(reason)
            }
        }
    }

    pub(super) fn lower_function_from(
        &mut self,
        subroutine: &sv::ir::Subroutine,
        key: &str,
        places: &[Place],
    ) -> Result<usize, Reason> {
        let process = self.m.process_state(&subroutine.name);
        // The simulation time is a module variable, which the unit reads.
        if process.time {
            self.m.time_slot();
        }
        let start = self.m.next_id;
        let (base, _) = self.m.temp("call_frame", 32, false, false);
        let (depth, _) = self.m.temp("call_depth", 32, false, false);
        // Only the automatic storage of the subroutine lives in its frame;
        // its static storage stays module variables (IEEE 1800-2023 13.4.2).
        let mut slots = vec![self.slot_of(base), self.slot_of(depth)];
        // The resume point and what a suspension leaves.
        let mut resume = None;
        let mut resume_slot = None;
        if process.suspends {
            resume_slot = Some(slots.len());
            let mut var = |purpose, width| {
                let (id, _) = self.m.temp(purpose, width, false, false);
                slots.push(self.slot_of(id));
                id
            };
            let ids = [
                var(
                    "call_resume",
                    celox_frontend_core::process::PROCESS_RESUME_WIDTH,
                ),
                var("call_status", celox_design::PROCESS_STATUS_WIDTH),
                var("call_request", celox_design::PROCESS_DELAY_WIDTH),
                var("call_top", celox_design::PROCESS_CALL_TOP_WIDTH),
            ];
            resume = Some(ResumeFrame {
                resume: ids[0],
                status: ids[1],
                request: ids[2],
                top: ids[3],
                blocks: Vec::new(),
                staging: HashSet::default(),
                calls: Vec::new(),
            });
        }
        // The calling process's state, which the caller passes in and,
        // for the RNG states, takes back.
        let mut random = None;
        let mut process_slots = (None, None);
        if process.random {
            process_slots.0 = Some((slots.len(), slots.len() + 1));
            let state = self.m.temp("call_random", 33, false, false);
            let (seed, seed_name) = self.m.temp("call_random_seed", 32, false, false);
            slots.push(self.slot_of(state.0));
            slots.push(self.slot_of(seed));
            random = Some((state, seed_name));
        }
        let mut dist_seed = None;
        if process.dist_seed {
            process_slots.1 = Some(slots.len());
            let seed = self.m.temp("call_dist_seed", 32, false, false);
            slots.push(self.slot_of(seed.0));
            dist_seed = Some(seed);
        }
        // The dynamic parts of the places of the `ref` formals (see
        // [`Place`]), which the caller passes.
        let mut place_slots = Vec::new();
        let mut renames = Vec::new();
        let mut aliases = Vec::new();
        let mut referents = HashSet::default();
        let references = subroutine
            .params
            .iter()
            .filter(|param| param.direction.is_reference());
        for (param, place) in references.zip(places) {
            let mut own = Vec::new();
            match place {
                Place::Variable(id) => {
                    renames.push((param.name.clone(), *id));
                    referents.insert(*id);
                }
                Place::Property { field, .. } => {
                    let (handle, handle_name) =
                        self.m
                            .temp(super::super::references::OBJECT_TEMP, 32, false, false);
                    self.m
                        .variables
                        .get_mut(&handle)
                        .expect("a temp")
                        .value_kind = celox_design::ValueKind::ObjRef;
                    own.push(slots.len());
                    slots.push(self.slot_of(handle));
                    aliases.push(super::super::references::Alias {
                        formal: param.name.clone(),
                        source_name: param.source_name.clone(),
                        target: super::super::references::Target::Property {
                            handle: handle_name,
                            field: *field,
                        },
                    });
                }
                Place::Element {
                    element,
                    signed,
                    width,
                    total,
                    positions,
                } => {
                    // The array is the caller's, which the unit writes.
                    if let sv::ir::LValue::Select { name, .. } = &**element
                        && let Some(id) = self.m.id(name)
                    {
                        referents.insert(id);
                    }
                    let mut names = HashMap::default();
                    for (k, caller) in positions.iter().enumerate() {
                        let position = self
                            .m
                            .id(caller)
                            .map(|id| self.m.var(id).clone())
                            .ok_or_else(|| Reason::Other(format!("position `{caller}`")))?;
                        let (temp, temp_name) = self.m.temp(
                            "position",
                            position.width,
                            position.signed,
                            position.is_4state,
                        );
                        own.push(slots.len());
                        slots.push(self.slot_of(temp));
                        names.insert(format!("#{k}"), temp_name);
                    }
                    let mut element = (**element).clone();
                    if let sv::ir::LValue::Select { msb, lsb, .. } = &mut element {
                        *msb = super::super::rename_const_idents(msb, &names);
                        *lsb = super::super::rename_const_idents(lsb, &names);
                    }
                    aliases.push(super::super::references::Alias {
                        formal: param.name.clone(),
                        source_name: param.source_name.clone(),
                        target: super::super::references::Target::Element {
                            element: Box::new(element),
                            signed: *signed,
                            width: *width,
                            total: *total,
                        },
                    });
                }
                Place::Frame { .. } => {
                    let id = self
                        .m
                        .id(&param.name)
                        .ok_or_else(|| Reason::Other(format!("formal `{}`", param.name)))?;
                    own.push(slots.len());
                    slots.push(self.slot_of(id));
                }
            }
            place_slots.push(own);
        }
        // The defaults of the input formals are evaluated by the unit, static
        // or automatic (see [`Function::omitted`]).
        let defaulted: Vec<usize> = subroutine
            .params
            .iter()
            .enumerate()
            .filter(|(position, param)| {
                *position < 64
                    && param.default.is_some()
                    && param.direction.is_read()
                    && !param.direction.is_written()
                    && !param.direction.is_reference()
                    && param.leaves.is_empty()
            })
            .map(|(position, _)| position)
            .collect();
        let mut omitted = None;
        let mut omitted_slot = None;
        if !defaulted.is_empty() {
            let (id, name) = self.m.temp("call_omitted", 64, false, false);
            omitted_slot = Some((slots.len(), defaulted.clone()));
            slots.push(self.slot_of(id));
            omitted = Some((name, defaulted));
        }
        // An automatic unpacked array has no frame slot: it stays a module
        // variable when no two activations of the unit can be live at once
        // (see `ProcModule::single_activation`).
        let single = self.m.single_activation(&subroutine.name);
        for name in automatic_names(subroutine) {
            let Some(id) = self.m.id(name) else {
                continue;
            };
            if slots.iter().any(|slot| slot.id == id) {
                continue;
            }
            if !self.m.var(id).array_dims.is_empty() && single {
                continue;
            }
            slots.push(self.slot_of(id));
        }
        let functions = self.m.out_of_line().expect("lowering out of line");
        let index = functions.functions.len();
        functions.functions.push(Function {
            key: key.to_string(),
            places: place_slots,
            slots,
            unit: None,
            size: 0,
            offsets: Vec::new(),
            reads: HashSet::default(),
            writes: HashSet::default(),
            callees: Vec::new(),
            size_patches: Vec::new(),
            random: process_slots.0,
            dist_seed: process_slots.1,
            resume: resume_slot,
            calls: Vec::new(),
            omitted: omitted_slot,
        });
        functions.by_name.insert(key.to_string(), index);
        functions.lowering += 1;
        let frame = FrameContext {
            function: index,
            base,
            depth,
            random,
            dist_seed,
            resume,
            size_patches: Vec::new(),
            renames,
            aliases,
            start,
            single,
            omitted,
        };
        let lowered = Ff::new(&mut *self.m, 0).lower_function_unit(subroutine, frame);
        let functions = self.m.out_of_line().expect("lowering out of line");
        functions.lowering -= 1;
        let (unit, size_patches, staging, calls) =
            lowered.map_err(|error| Reason::Other(error.to_string()))?;
        self.finish_function(
            index,
            unit,
            size_patches,
            start,
            &staging,
            &referents,
            single,
        )?;
        let functions = self.m.out_of_line().expect("lowering out of line");
        functions.functions[index].calls = calls;
        Ok(index)
    }

    pub(super) fn slot_of(&mut self, id: SourceVarId) -> Slot {
        let functions = self.m.out_of_line().expect("lowering out of line");
        // Frame addresses are a namespace of their own (the FRAME region):
        // numbering them from the top of the id space keeps them apart from
        // the module's variables in dumps.
        let address = SourceVarId(u32::MAX / 2 + functions.slots);
        functions.slots += 1;
        let variable = self.m.var(id);
        Slot {
            id,
            address,
            width: variable.width,
            four_state: variable.is_4state && self.m.four_state,
            kind: variable.value_kind,
            element_width: super::super::super::unpacked_element_width(variable),
        }
    }

    /// Lower the body of `subroutine` as the unit of function `index`, in
    /// a fresh [`Ff`].
    pub(super) fn lower_function_unit(
        mut self,
        subroutine: &sv::ir::Subroutine,
        frame: FrameContext,
    ) -> Result<
        (
            ExecutionUnit<Addr>,
            Vec<(RegisterId, usize)>,
            HashSet<SourceVarId>,
            Vec<celox_design::CallResume>,
        ),
        sv::AnalyzerError,
    > {
        self.direct = true;
        // The body draws on the RNG states in the frame, which the caller
        // passes.
        let (random, dist_seed) = (frame.random.is_some(), frame.dist_seed.is_some());
        self.rng.random = frame.random.as_ref().map(|(state, _)| state.clone());
        self.rng.dist_seed = frame.dist_seed.clone();
        self.frame = Some(frame);
        let start = self.b.current_block();
        if let Some(resume) = self.resume_frame() {
            resume.blocks.push(start);
        }
        let return_block = self.b.new_block();
        self.scope.functions.push(FunctionBlocks {
            return_block,
            return_var: subroutine.return_var.clone(),
            loop_depth: 0,
            block_depth: 0,
        });
        if let Some(return_var) = &subroutine.return_var
            && !subroutine.persistent_locals.contains(return_var)
        {
            let id = self
                .m
                .id(return_var)
                .ok_or_else(|| unsupported(format!("result of `{}`", subroutine.name)))?;
            self.init_default(id);
        }
        // The body names the variable of each `ref` formal bound to one,
        // and reads and writes the property or element of the others
        // through the slots of the handle and positions.
        let (renames, aliases) = self
            .frame
            .as_mut()
            .map(|frame| {
                (
                    std::mem::take(&mut frame.renames),
                    std::mem::take(&mut frame.aliases),
                )
            })
            .unwrap_or_default();
        let mut body = subroutine.body.clone();
        // A formal the call omits takes its default, evaluated here.
        if let Some((mask, positions)) = self.frame.as_mut().and_then(|frame| frame.omitted.take())
        {
            let prologue = positions.into_iter().map(|position| {
                let param = &subroutine.params[position];
                let bit = sv::ir::ConstExpr::Literal(position.to_string());
                sv::ir::Stmt::If {
                    condition: sv::ir::Expr::Select {
                        expr: Box::new(sv::ir::Expr::Ident(mask.clone())),
                        msb: bit.clone(),
                        lsb: bit,
                        signed: false,
                    },
                    then_body: vec![sv::ir::Stmt::Assign {
                        lhs: sv::ir::LValue::Ident(param.name.clone()),
                        rhs: param.default.clone().expect("a defaulted formal"),
                        nonblocking: false,
                    }],
                    else_body: Vec::new(),
                }
            });
            body.splice(0..0, prologue);
        }
        let (constants, parameter_types) = (self.m.constants, self.m.parameter_types);
        let constant = |position: &sv::ir::ConstExpr| {
            sv::typecheck::eval_const_expr_with_types(position, constants, parameter_types)
        };
        self.split_property_concats(&mut body, &aliases);
        for alias in &aliases {
            body = alias.rewrite(body, &constant, &mut |field| self.part_scratch(field))?;
        }
        let (body, shared) = self.share_forked(subroutine, body)?;
        let originals: Vec<(String, Option<SourceVarId>)> = renames
            .iter()
            .map(|(formal, target)| (formal.clone(), self.m.bind_name(formal.clone(), *target)))
            .collect();
        self.call_stack.active_calls.push(subroutine.name.clone());
        let falls = self.exec_block(&body);
        self.call_stack
            .shared_boxes
            .truncate(self.call_stack.shared_boxes.len() - shared);
        for (formal, original) in originals {
            match original {
                Some(id) => {
                    self.m.bind_name(formal, id);
                }
                None => self.m.unbind_name(&formal),
            }
        }
        if falls? {
            self.jump(return_block);
        }
        self.b.switch_to_block(return_block);
        if let Some(status) = self.resume_frame().map(|resume| resume.status) {
            let done = self.b.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
            self.b.emit(SIRInstruction::Imm(done, SIRValue::new(0u8)));
            self.store(
                status,
                SIROffset::Static(0),
                celox_design::PROCESS_STATUS_WIDTH,
                done,
            );
        }
        self.b.seal_block(SIRTerminator::Return);
        // The entry continues the body where the activation last suspended.
        let mut entry = start;
        if let Some((resume, blocks)) = self
            .resume_frame()
            .map(|resume| (resume.resume, resume.blocks.clone()))
        {
            let invalid = self.b.new_block();
            self.b.switch_to_block(invalid);
            let code = self
                .m
                .runtime_error(format!("invalid resume point of `{}`", subroutine.name));
            self.b.seal_block(SIRTerminator::Error(code));
            entry = self.b.new_block();
            self.b.switch_to_block(entry);
            let point = self
                .b
                .alloc_bit(celox_frontend_core::process::PROCESS_RESUME_WIDTH, false);
            self.b.emit(SIRInstruction::Load(
                point,
                stable(resume),
                SIROffset::Static(0),
                celox_frontend_core::process::PROCESS_RESUME_WIDTH,
            ));
            celox_frontend_core::process::resume_dispatch(&mut self.b, point, &blocks, invalid);
        }
        // The eligibility check keeps out what needs the state of a process;
        // this is the net under it.
        if !self.sites.host_request.is_empty()
            || !self.sites.wait.is_empty()
            || !self.sites.nba.is_empty()
            || !self.sites.event_probes.is_empty()
            || (self.rng.random.is_some() && !random)
            || (self.rng.dist_seed.is_some() && !dist_seed)
        {
            return Err(unsupported("process state in a function unit"));
        }
        let unit = seal_drained_from(std::mem::take(&mut self.b), entry);
        let frame = self.frame.take().expect("lowering a function unit");
        let (staging, calls) = frame
            .resume
            .map(|resume| (resume.staging, resume.calls))
            .unwrap_or_default();
        Ok((unit, frame.size_patches, staging, calls))
    }

    /// Move the frame variables of function `index` into its frame, add
    /// the temporaries its body created (`start` and up) to the frame, and
    /// check that the unit touches no other state than it may.
    pub(super) fn finish_function(
        &mut self,
        index: usize,
        mut unit: ExecutionUnit<Addr>,
        size_patches: Vec<(RegisterId, usize)>,
        start: SourceVarId,
        staging: &HashSet<SourceVarId>,
        referents: &HashSet<SourceVarId>,
        single: bool,
    ) -> Result<(), Reason> {
        let end = self.m.next_id;
        let functions = self.m.out_of_line().expect("lowering out of line");
        let mut slots = functions.functions[index].slots.clone();
        let mut accessed = Vec::new();
        for block in unit.blocks.values() {
            for instruction in &block.instructions {
                accessed.extend(instruction.memory_read().map(|access| access.addr.var_id));
                accessed.extend(instruction.memory_write().map(|access| access.addr.var_id));
            }
        }
        accessed.sort_unstable();
        accessed.dedup();
        // The automatic storage of the subroutines inlined into the body:
        // each activation needs its own as much as the function's.
        self.m.check_storage_index();
        // The automatic arrays that stay module variables.
        let mut in_place = HashSet::default();
        for id in accessed {
            // The variable a `ref` formal names is the caller's.
            if slots.iter().any(|slot| slot.id == id)
                || staging.contains(&id)
                || referents.contains(&id)
            {
                continue;
            }
            let created = (start.0..end.0).contains(&id.0);
            if !created && !self.m.is_automatic_storage(id) {
                continue;
            }
            let variable = self.m.var(id);
            if created && (!variable.hidden || !variable.path[0].contains("@t")) {
                return Err(Reason::Other(
                    "module state created in a function".to_string(),
                ));
            }
            // The automatic arrays of the bodies the unit inlines too: they
            // are live only within an activation of the unit.
            if !variable.array_dims.is_empty() && single && !created {
                in_place.insert(id);
                continue;
            }
            slots.push(self.slot_of(id));
        }
        // A function may write the module's variables, static storage of
        // subroutines and static class properties among them, which every
        // call shares, inlined or not; and the variables its `ref` formals
        // name, which the caller passes, such as the locals of a process
        // (the place selects the function, so only calls passing them reach
        // it). Not the other hidden ones the lowering keeps for a process,
        // nor what is observed: assembly instruments the stores into that,
        // which only the stores of the units the runtime runs can be.
        let frame: HashMap<SourceVarId, (usize, SourceVarId, Option<usize>)> = slots
            .iter()
            .map(|slot| (slot.id, (slot.width, slot.address, slot.element_width)))
            .collect();
        let mut reads = HashSet::default();
        let mut writes = HashSet::default();
        let mut callees = Vec::new();
        let other = |what: &str| Reason::Other(what.to_string());
        for block in unit.blocks.values_mut() {
            for instruction in &mut block.instructions {
                let (addr, offset, width, store) = match instruction {
                    SIRInstruction::Load(_, addr, offset, width) => (addr, &*offset, *width, false),
                    SIRInstruction::Store(addr, offset, width, _, triggers, captures) => {
                        if !triggers.is_empty() || !captures.is_empty() {
                            return Err(other("instrumented store"));
                        }
                        (addr, &*offset, *width, true)
                    }
                    SIRInstruction::Commit(..) => return Err(other("commit")),
                    SIRInstruction::RuntimeEvent { .. } => continue,
                    SIRInstruction::CombCaptureEvent { .. }
                    | SIRInstruction::CombCaptureEnableIfChanged { .. } => {
                        return Err(Reason::HostRequest);
                    }
                    SIRInstruction::ExternCall { func, .. }
                        if *func != celox_sir::extern_abi::HEAP_HELPER_ID =>
                    {
                        return Err(Reason::HostRequest);
                    }
                    SIRInstruction::Call { function, .. } => {
                        callees.push(function.0 as usize);
                        continue;
                    }
                    instruction if instruction.accesses_unknown_state() => {
                        return Err(other("kernel call"));
                    }
                    _ => continue,
                };
                if let Some(&(slot_width, address, element)) = frame.get(&addr.var_id) {
                    // A slot holding an array is also accessed at positions
                    // computed at run time, within one element, as the
                    // lowering accesses an array of the state.
                    match (offset, element) {
                        (SIROffset::Static(offset), _)
                        | (
                            SIROffset::PackedElements {
                                bit_offset: offset, ..
                            },
                            Some(_),
                        ) if offset + width <= slot_width => {}
                        (
                            SIROffset::Element {
                                element_width,
                                bit_offset,
                                dynamic_bit_offset,
                                ..
                            },
                            Some(element),
                        ) if *element_width == element
                            && width <= element
                            && (dynamic_bit_offset.is_some() || bit_offset + width <= element) => {}
                        (SIROffset::Dynamic(_), Some(element)) if width <= element => {}
                        _ => return Err(other("dynamic frame access")),
                    }
                    *addr = frame_addr(address);
                } else if addr.region != STABLE_REGION {
                    return Err(other("non-stable state access"));
                } else if store {
                    if self.m.is_hidden(addr.var_id)
                        && !self.m.is_static_storage(addr.var_id)
                        && !staging.contains(&addr.var_id)
                        && !in_place.contains(&addr.var_id)
                        && !referents.contains(&addr.var_id)
                        && !self.m.box_changes.contains(&addr.var_id)
                    {
                        return Err(other("writes hidden module state"));
                    }
                    if self.m.is_excluded(addr.var_id) {
                        return Err(other("stores into observed storage"));
                    }
                    writes.insert(addr.var_id);
                } else {
                    reads.insert(addr.var_id);
                }
            }
        }
        let (offsets, size) = layout(&slots);
        if size > celox_sir::STACK_SIZE {
            return Err(other("frame larger than the stack"));
        }
        let functions = self.m.out_of_line().expect("lowering out of line");
        let function = &mut functions.functions[index];
        function.slots = slots;
        function.offsets = offsets;
        function.size = size;
        function.reads = reads;
        function.writes = writes;
        function.callees = callees;
        function.size_patches = size_patches;
        function.unit = Some(unit);
        Ok(())
    }
}
