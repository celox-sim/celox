//! Suspension of a function unit: its resume points, waits, nonblocking
//! updates and host requests.

use super::*;

impl Ff<'_, '_> {
    /// A nonblocking update of bits `lsb..lsb + width` of `target`, its
    /// value in variable `staged` and its run-time position, if any, in
    /// `position`, from the unit being lowered: the value and position go
    /// into the staging objects of a new function NBA site, and the unit
    /// suspends with [`celox_design::ProcessStatus::Nba`], whose word the
    /// calling kernel turns into its process's site.
    pub(in crate::lowering::ff) fn unit_nonblocking(
        &mut self,
        delay: u64,
        target: SourceVarId,
        lsb: usize,
        width: usize,
        staged: SourceVarId,
        position: Option<NbaPosition<SourceVarId>>,
    ) -> Result<(), sv::AnalyzerError> {
        use celox_frontend_core::process::NBA_POSITION_WIDTH;
        if !self.can_suspend_unit() {
            return Err(unsupported(
                "a nonblocking assignment in a unit that cannot suspend",
            ));
        }
        let four_state = self.m.var(staged).is_4state;
        let (value, _) = self.m.temp("nba_value", width, false, four_state);
        self.copy_variable(staged, value);
        let slots = match position {
            Some(position) => {
                let offset = self
                    .m
                    .temp("nba_offset", NBA_POSITION_WIDTH, false, false)
                    .0;
                let width = self.m.temp("nba_width", NBA_POSITION_WIDTH, false, false).0;
                self.copy_variable(position.offset, offset);
                self.copy_variable(position.width, width);
                Some(NbaPosition { offset, width })
            }
            None => None,
        };
        if let Some(resume) = self.resume_frame() {
            resume.staging.insert(value);
            if let Some(slots) = slots {
                resume.staging.extend([slots.offset, slots.width]);
            }
        }
        let index = self.m.function_sites.nba.len() as u64;
        self.m.function_sites.nba.push(NbaSite {
            delay,
            process: 0,
            target,
            lsb,
            width,
            value,
            position: slots,
            pending: None,
            unit: false,
        });
        let request = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
        self.b
            .emit(SIRInstruction::Imm(request, SIRValue::new(index)));
        self.suspend_unit_with(celox_design::ProcessStatus::Nba, request)
    }

    /// Whether a unit with resume slots is being lowered, which can
    /// suspend the process it runs in.
    pub(in crate::lowering::ff) fn can_suspend_unit(&self) -> bool {
        self.frame
            .as_ref()
            .is_some_and(|frame| frame.resume.is_some())
    }

    /// The end of the frame of the unit being lowered, function `own`
    /// whose frame starts at the value of `base`.
    pub(super) fn own_top(&mut self, base: SourceVarId, own: usize) -> RegisterId {
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

    /// Suspend the unit being lowered with the delay word `word` of a wait
    /// at one of the module's function wait sites (see
    /// `FunctionSites::wait`); the open block afterwards is the
    /// resume point, whose value it returns.
    pub(in crate::lowering::ff) fn unit_wait(
        &mut self,
        word: u64,
    ) -> Result<u32, sv::AnalyzerError> {
        let request = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
        self.b
            .emit(SIRInstruction::Imm(request, SIRValue::new(word)));
        self.unit_wait_word(request)
    }

    /// [`Self::unit_wait`] with the delay word in register `request`.
    pub(super) fn unit_wait_word(&mut self, request: RegisterId) -> Result<u32, sv::AnalyzerError> {
        let status = self.b.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
        self.b.emit(SIRInstruction::Imm(
            status,
            SIRValue::new(celox_design::ProcessStatus::Wait.code()),
        ));
        self.suspend_unit(status, request, None)
    }

    /// The process continues after a wait in the unit being lowered: a
    /// flush point of its deferred assertions (IEEE 1800-2023 16.4.2), as
    /// a kernel's resume from a wait is. The site is the module's one for
    /// every unit; it flushes the queue of the process the unit runs in.
    pub(in crate::lowering::ff) fn unit_flush_point(&mut self) {
        let site = match self.m.function_sites.flush {
            Some(site) => site,
            None => {
                let site = self.m.event_site(RuntimeEventSite {
                    kind: RuntimeEventKind::Write,
                    template: None,
                    sizing: DisplaySizing::Ieee,
                    scope: None,
                    location: None,
                    assertion: None,
                    deferred: Some(DeferredSite::ProcessFlushPoint),
                    timestamp_arg: false,
                    time_format_arg: false,
                    arg_widths: Vec::new(),
                    arg_signed: Vec::new(),
                    arg_is_string: Vec::new(),
                    arg_float_precision: Vec::new(),
                    arg_is_handle: Vec::new(),
                });
                self.m.function_sites.flush = Some(site);
                site
            }
        };
        self.b.emit(SIRInstruction::RuntimeEvent {
            site_id: site,
            args: Vec::new(),
        });
    }

    /// At resume point `point` of a wait with delay word `word`: continue
    /// when `woken`, a one-bit register, holds; otherwise report that the
    /// wait is still pending and return to the same point, as a kernel's
    /// `wake_if` does. The continuation is open afterwards.
    pub(in crate::lowering::ff) fn unit_wake_if(
        &mut self,
        woken: RegisterId,
        point: u32,
        word: u64,
    ) -> Result<(), sv::AnalyzerError> {
        let frame = self.frame.as_ref().expect("lowering a function unit");
        let (own, base) = (frame.function, frame.base);
        let resume = frame
            .resume
            .as_ref()
            .ok_or_else(|| unsupported("a wait in a unit that cannot suspend"))?;
        let (resume_var, status_var, request_var, top_var) =
            (resume.resume, resume.status, resume.request, resume.top);
        let continuation = self.b.new_block();
        let pending = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: woken,
            true_block: (continuation, Vec::new()),
            false_block: (pending, Vec::new()),
        });
        self.b.switch_to_block(pending);
        let width = celox_frontend_core::process::PROCESS_RESUME_WIDTH;
        let point_value = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Imm(
            point_value,
            SIRValue::new(u64::from(point)),
        ));
        self.store(resume_var, SIROffset::Static(0), width, point_value);
        let status = self.b.alloc_bit(celox_design::PROCESS_STATUS_WIDTH, false);
        self.b.emit(SIRInstruction::Imm(
            status,
            SIRValue::new(celox_design::ProcessStatus::Pending.code()),
        ));
        self.store(
            status_var,
            SIROffset::Static(0),
            celox_design::PROCESS_STATUS_WIDTH,
            status,
        );
        let request = self.b.alloc_bit(celox_design::PROCESS_DELAY_WIDTH, false);
        self.b
            .emit(SIRInstruction::Imm(request, SIRValue::new(word)));
        self.store(
            request_var,
            SIROffset::Static(0),
            celox_design::PROCESS_DELAY_WIDTH,
            request,
        );
        let top = self.own_top(base, own);
        self.store(
            top_var,
            SIROffset::Static(0),
            celox_design::PROCESS_CALL_TOP_WIDTH,
            top,
        );
        self.b.seal_block(SIRTerminator::Return);
        self.b.switch_to_block(continuation);
        Ok(())
    }

    /// Stage module variable `id` in the resumable unit being lowered: a
    /// count or gate that every process shares, which the unit reads and
    /// writes in place.
    pub(in crate::lowering::ff) fn stage(&mut self, id: SourceVarId) {
        if let Some(resume) = self.resume_frame() {
            resume.staging.insert(id);
        }
    }

    pub(super) fn resume_frame(&mut self) -> Option<&mut ResumeFrame> {
        self.frame.as_mut().and_then(|frame| frame.resume.as_mut())
    }

    /// Whether the code being lowered may make a host request: a process
    /// kernel, or a unit that suspends the process for it.
    pub(in crate::lowering::ff) fn can_request(&self) -> bool {
        self.kernel.is_some()
            || self
                .frame
                .as_ref()
                .is_some_and(|frame| frame.resume.is_some())
    }

    /// Make host request `site` of family `kind`, which the lowering just
    /// added to [`Self::host_request_sites`]: suspend the process with it,
    /// and continue when the host has served it.
    pub(in crate::lowering::ff) fn request_host(
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
            .sites
            .host_request
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
            .function_sites
            .host_request
            .iter()
            .filter(|known| known.kind == kind)
            .count() as u32;
        self.m.function_sites.host_request.push(request);
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
    pub(in crate::lowering::ff) fn suspend_unit_with(
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
    pub(in crate::lowering::ff) fn copy_variable(&mut self, from: SourceVarId, to: SourceVarId) {
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
    pub(super) fn suspend_unit(
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
            None => self.own_top(base, own),
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
}
