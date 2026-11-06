//! Out-of-line calls (see [`super::super::out_of_line`]): the call sites,
//! and the lowering of a subroutine body into its function unit.

use super::super::out_of_line::{
    Function, Outcome, Reason, Slot, automatic_names, frame_addr, layout,
};
use super::*;

/// The function whose unit an [`Ff`] is lowering.
pub(super) struct FrameContext {
    /// Its index among the module's functions.
    function: usize,
    /// The variables of the slots that hold the frame's base and the call
    /// depth of the activation.
    base: SourceVarId,
    depth: SourceVarId,
    /// The variables of the slots of the calling process's state that the
    /// body uses (see [`super::super::out_of_line::ProcessState`]): the RNG
    /// state and the name of the process seed, and the seed of `$random`.
    pub(super) random: Option<((SourceVarId, String), String)>,
    pub(super) dist_seed: Option<(SourceVarId, String)>,
    /// For a resumable unit (see [`Function::resume`]), its resume slots.
    resume: Option<ResumeFrame>,
    /// Registers holding a frame size not known yet (see
    /// [`Function::size_patches`]).
    size_patches: Vec<(RegisterId, usize)>,
}

/// The resume slots of a resumable unit being lowered, and its resume
/// points.
struct ResumeFrame {
    resume: SourceVarId,
    status: SourceVarId,
    request: SourceVarId,
    top: SourceVarId,
    /// The block each resume point continues in; point 0 is the start.
    blocks: Vec<BlockId>,
    /// The module variables through which its host requests pass their
    /// values: the host reads and writes them while the unit is suspended,
    /// which only its innermost activation can be, and the unit copies them
    /// from and into its frame right before and after.
    staging: HashSet<SourceVarId>,
    /// The calls at which it can be suspended inside the callee.
    calls: Vec<celox_design::CallResume>,
}

impl Ff<'_, '_> {
    /// The function a call of `name` enters, when the call goes out of line.
    pub(super) fn out_of_line_callee(
        &mut self,
        name: &str,
        subroutine: &sv::ir::Subroutine,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<Option<usize>, sv::AnalyzerError> {
        if self.m.out_of_line().is_none() {
            return Ok(None);
        }
        if let Some(reason) = self.m.out_of_line_reason(name) {
            self.record_outcome(name, Outcome::Inlined(reason));
            return Ok(None);
        }
        // Code that must not call, such as the probe of an event expression,
        // and a call that inlining folds, unless it is recursive and so
        // cannot be inlined.
        let recursive = self.active_calls.iter().any(|active| active == name);
        if self.inline_calls {
            self.record_outcome(
                name,
                Outcome::Inlined(Reason::Other("event expression".to_string())),
            );
            return Ok(None);
        }
        if !recursive && self.m.constant_arguments(subroutine, args) {
            self.record_outcome(
                name,
                Outcome::Inlined(Reason::Other("constant arguments".to_string())),
            );
            return Ok(None);
        }
        let Some(functions) = self.m.out_of_line() else {
            return Ok(None);
        };
        if let Some(reason) = functions.rejected.get(name).cloned() {
            self.record_outcome(name, Outcome::Inlined(reason));
            return Ok(None);
        }
        let index = match functions.by_name.get(name).copied() {
            Some(index) => index,
            None => match self.lower_function(name) {
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
                        !self.working.contains(id)
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
            // subroutines, which the process does not own.
            let persistent = self.m.static_storage();
            if touched.is_some_and(|(_, writes)| writes.iter().any(|id| !persistent.contains(id))) {
                self.record_outcome(
                    name,
                    Outcome::Inlined(Reason::Other(
                        "module state in a combinational process".to_string(),
                    )),
                );
                return Ok(None);
            }
        }
        self.record_outcome(name, Outcome::OutOfLine);
        Ok(Some(index))
    }

    fn record_outcome(&mut self, name: &str, outcome: Outcome) {
        if let Some(functions) = self.m.out_of_line() {
            functions.record(name, outcome);
        }
    }

    /// Lower subroutine `name` into a new function unit, or say why it
    /// cannot be. A failed lowering leaves nothing behind, so the call is
    /// inlined as if it had never been tried.
    fn lower_function(&mut self, name: &str) -> Result<usize, Reason> {
        let subroutine = self
            .m
            .subroutine(name)
            .cloned()
            .ok_or_else(|| Reason::Other(format!("no subroutine `{name}`")))?;
        let checkpoint = self.m.checkpoint();
        let names = self.m.declare_names();
        let lowered = self.lower_function_from(&subroutine);
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
                    functions.rejected.insert(name.to_string(), reason.clone());
                }
                Err(reason)
            }
        }
    }

    fn lower_function_from(&mut self, subroutine: &sv::ir::Subroutine) -> Result<usize, Reason> {
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
        for name in automatic_names(subroutine) {
            let Some(id) = self.m.id(name) else {
                continue;
            };
            if slots.iter().any(|slot| slot.id == id) {
                continue;
            }
            if !self.m.var(id).array_dims.is_empty() {
                return Err(Reason::Other("unpacked array in the frame".to_string()));
            }
            slots.push(self.slot_of(id));
        }
        let functions = self.m.out_of_line().expect("lowering out of line");
        let index = functions.functions.len();
        functions.functions.push(Function {
            name: subroutine.name.clone(),
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
        });
        functions.by_name.insert(subroutine.name.clone(), index);
        functions.lowering += 1;
        let frame = FrameContext {
            function: index,
            base,
            depth,
            random,
            dist_seed,
            resume,
            size_patches: Vec::new(),
        };
        let lowered = Ff::new(&mut *self.m, 0).lower_function_unit(subroutine, frame);
        let functions = self.m.out_of_line().expect("lowering out of line");
        functions.lowering -= 1;
        let (unit, size_patches, staging, calls) =
            lowered.map_err(|error| Reason::Other(error.to_string()))?;
        self.finish_function(index, unit, size_patches, start, &staging)?;
        let functions = self.m.out_of_line().expect("lowering out of line");
        functions.functions[index].calls = calls;
        Ok(index)
    }

    fn slot_of(&mut self, id: SourceVarId) -> Slot {
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
        }
    }

    /// Lower the body of `subroutine` as the unit of function `index`, in
    /// a fresh [`Ff`].
    fn lower_function_unit(
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
        self.random = frame.random.as_ref().map(|(state, _)| state.clone());
        self.dist_seed = frame.dist_seed.clone();
        self.frame = Some(frame);
        let start = self.b.current_block();
        if let Some(resume) = self.resume_frame() {
            resume.blocks.push(start);
        }
        let return_block = self.b.new_block();
        self.functions.push(FunctionBlocks {
            return_block,
            return_var: subroutine.return_var.clone(),
            loop_depth: 0,
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
        self.active_calls.push(subroutine.name.clone());
        if self.exec_block(&subroutine.body)? {
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
        if !self.host_request_sites.is_empty()
            || !self.wait_sites.is_empty()
            || !self.nba_sites.is_empty()
            || !self.event_probes.is_empty()
            || (self.random.is_some() && !random)
            || (self.dist_seed.is_some() && !dist_seed)
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

    fn resume_frame(&mut self) -> Option<&mut ResumeFrame> {
        self.frame.as_mut().and_then(|frame| frame.resume.as_mut())
    }

    /// Whether the code being lowered may make a host request: a process
    /// kernel, or a unit that suspends the process for it.
    pub(super) fn can_request(&self) -> bool {
        self.kernel.is_some()
            || self
                .frame
                .as_ref()
                .is_some_and(|frame| frame.resume.is_some())
    }

    /// Make host request `site` of family `kind`, which the lowering just
    /// added to [`Self::host_request_sites`]: suspend the process with it,
    /// and continue when the host has served it.
    pub(super) fn request_host(
        &mut self,
        kind: celox_design::HostRequestKind,
        site: u32,
    ) -> Result<(), sv::AnalyzerError> {
        if self.kernel.is_some() {
            return self
                .with_kernel(|kernel| kernel.host_request(kind, site))
                .map_err(kernel_error);
        }
        // In a unit, the site is one of the module's function sites, and
        // its values pass through staging variables (see
        // [`ResumeFrame::staging`]).
        let mut request = self
            .host_request_sites
            .pop()
            .filter(|request| request.kind == kind)
            .ok_or_else(|| unsupported("a host request without its site"))?;
        let mut staged = Vec::new();
        for value in request.args.iter_mut().chain(&mut request.results) {
            let variable = self.m.var(value.id).clone();
            let (stage, _) = self.m.temp(
                "call_stage",
                variable.width,
                variable.signed,
                variable.is_4state,
            );
            self.m
                .variables
                .get_mut(&stage)
                .expect("staging")
                .value_kind = variable.value_kind;
            self.copy_variable(value.id, stage);
            staged.push((value.id, stage));
            value.id = stage;
        }
        let results: Vec<(SourceVarId, SourceVarId)> = staged[request.args.len()..].to_vec();
        if let Some(resume) = self.resume_frame() {
            resume
                .staging
                .extend(staged.iter().map(|&(_, stage)| stage));
        }
        let number = self
            .m
            .function_host_request_sites
            .iter()
            .filter(|known| known.kind == kind)
            .count() as u32;
        self.m.function_host_request_sites.push(request);
        let status = self.b.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
        self.b.emit(SIRInstruction::Imm(
            status,
            SIRValue::new(celox_design::ProcessStatus::HostRequest.code()),
        ));
        let word = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
        self.b.emit(SIRInstruction::Imm(
            word,
            SIRValue::new(kind.encode(celox_design::FUNCTION_SITE_NUMBER | number)),
        ));
        self.suspend_unit(status, word, None)?;
        for (id, stage) in results {
            self.copy_variable(stage, id);
        }
        Ok(())
    }

    /// Suspend the unit being lowered with `status` and the delay word
    /// `request`, as a kernel suspends itself: for a delay, `$stop` or
    /// `$finish`. The open block afterwards is the resume point.
    pub(super) fn suspend_unit_with(
        &mut self,
        status: celox_design::ProcessStatus,
        request: RegisterId,
    ) -> Result<(), sv::AnalyzerError> {
        let code = self.b.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
        self.b
            .emit(SIRInstruction::Imm(code, SIRValue::new(status.code())));
        self.suspend_unit(code, request, None).map(drop)
    }

    /// Store variable `from` into variable `to`, of the same shape.
    fn copy_variable(&mut self, from: SourceVarId, to: SourceVarId) {
        let variable = self.m.var(from);
        let width = variable.width;
        let value = if variable.is_4state && self.m.four_state {
            self.b.alloc_logic(width)
        } else {
            self.b.alloc_bit(width, variable.signed)
        };
        self.b.emit(SIRInstruction::Load(
            value,
            stable(from),
            SIROffset::Static(0),
            width,
        ));
        self.store(to, SIROffset::Static(0), width, value);
    }

    /// Suspend the unit being lowered with `status` and the delay word
    /// `request`, leaving `top` as the end of the innermost live frame, its
    /// own when `None`. The open block afterwards is the resume point,
    /// whose value it returns.
    fn suspend_unit(
        &mut self,
        status: RegisterId,
        request: RegisterId,
        top: Option<RegisterId>,
    ) -> Result<u32, sv::AnalyzerError> {
        let frame = self.frame.as_ref().expect("lowering a function unit");
        let (own, base) = (frame.function, frame.base);
        let resume = frame
            .resume
            .as_ref()
            .ok_or_else(|| unsupported("a suspension in a unit that cannot suspend"))?;
        let (resume_var, status_var, request_var, top_var) =
            (resume.resume, resume.status, resume.request, resume.top);
        let point = resume.blocks.len();
        if point > celox_frontend_core::process::MAX_PROCESS_SUSPENSIONS {
            return Err(unsupported("a function with too many suspension points"));
        }
        let top = match top {
            Some(top) => top,
            None => {
                let current = self.b.alloc_bit(32, false);
                self.b.emit(SIRInstruction::Load(
                    current,
                    stable(base),
                    SIROffset::Static(0),
                    32,
                ));
                // The frame size is patched once known.
                let size = self.b.alloc_bit(32, false);
                self.b.emit(SIRInstruction::Imm(size, SIRValue::new(0u32)));
                self.frame
                    .as_mut()
                    .expect("lowering a function unit")
                    .size_patches
                    .push((size, own));
                let end = self.b.alloc_bit(32, false);
                self.b
                    .emit(SIRInstruction::Binary(end, current, BinaryOp::Add, size));
                end
            }
        };
        let width = celox_frontend_core::process::PROCESS_RESUME_WIDTH;
        let point_value = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Imm(
            point_value,
            SIRValue::new(point as u64),
        ));
        self.store(resume_var, SIROffset::Static(0), width, point_value);
        self.store(
            status_var,
            SIROffset::Static(0),
            celox_design::PROCESS_STATUS_WIDTH,
            status,
        );
        self.store(
            request_var,
            SIROffset::Static(0),
            celox_design::PROCESS_DELAY_WIDTH,
            request,
        );
        self.store(
            top_var,
            SIROffset::Static(0),
            celox_design::PROCESS_CALL_TOP_WIDTH,
            top,
        );
        self.b.seal_block(SIRTerminator::Return);
        let block = self.b.new_block();
        self.resume_frame()
            .expect("checked above")
            .blocks
            .push(block);
        self.b.switch_to_block(block);
        Ok(point as u32)
    }

    /// Move the frame variables of function `index` into its frame, add
    /// the temporaries its body created (`start` and up) to the frame, and
    /// check that the unit touches no other state than it may.
    fn finish_function(
        &mut self,
        index: usize,
        mut unit: ExecutionUnit<Addr>,
        size_patches: Vec<(RegisterId, usize)>,
        start: SourceVarId,
        staging: &HashSet<SourceVarId>,
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
        let automatic = self.m.automatic_storage();
        for id in accessed {
            if slots.iter().any(|slot| slot.id == id) || staging.contains(&id) {
                continue;
            }
            let created = (start.0..end.0).contains(&id.0);
            if !created && !automatic.contains(&id) {
                continue;
            }
            let variable = self.m.var(id);
            if created && (!variable.hidden || !variable.path[0].contains("@t")) {
                return Err(Reason::Other(
                    "module state created in a function".to_string(),
                ));
            }
            if !variable.array_dims.is_empty() {
                return Err(Reason::Other("unpacked array in the frame".to_string()));
            }
            slots.push(self.slot_of(id));
        }
        // A function may write the module's variables, static storage of
        // subroutines and static class properties among them, which every
        // call shares, inlined or not; not the hidden ones the lowering
        // keeps for a process, nor what is observed: assembly instruments
        // the stores into that, which only the stores of the units the
        // runtime runs can be.
        let persistent = self.m.static_storage();
        let frame: HashMap<SourceVarId, (usize, SourceVarId)> = slots
            .iter()
            .map(|slot| (slot.id, (slot.width, slot.address)))
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
                if let Some(&(slot_width, address)) = frame.get(&addr.var_id) {
                    match offset {
                        SIROffset::Static(offset) if offset + width <= slot_width => {}
                        _ => return Err(other("dynamic frame access")),
                    }
                    *addr = frame_addr(address);
                } else if addr.region != STABLE_REGION {
                    return Err(other("non-stable state access"));
                } else if store {
                    if self.m.is_hidden(addr.var_id)
                        && !persistent.contains(&addr.var_id)
                        && !staging.contains(&addr.var_id)
                    {
                        return Err(other("writes hidden module state"));
                    }
                    if self.m.is_observed(addr.var_id) {
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

    /// A hidden variable of the shape of `id`, which receives a value that
    /// a call leaves in the slot of `id`. A call made in a function unit
    /// thereby keeps it in its own frame, apart from the callee's.
    pub(super) fn call_result_temp(&mut self, id: SourceVarId) -> (SourceVarId, String) {
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
                ..variable
            },
        );
        (copy, name)
    }

    /// Call function `index`, the unit of `subroutine`: pass `inputs`, the
    /// values of the formals the caller writes, store each `(formal, temp)`
    /// of `copies` from the formal's slot after the call, and return the
    /// variable that holds the result.
    pub(super) fn call_out_of_line(
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
            let word = load(&mut self.b, request);
            let end = load(&mut self.b, top);
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
                self.call_resumes.push(celox_design::CallResume {
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
    fn process_args(
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
    fn emit_call(
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
    fn call_frame(
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
    fn raise_if(&mut self, condition: RegisterId, message: String) {
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
