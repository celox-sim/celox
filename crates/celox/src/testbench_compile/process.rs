//! Veryl statements lowered with the same resumable SIR builder as SV.
use crate::ir::{AbsoluteAddr as Addr, RuntimeProgram, SirProgram};
use celox_design::{
    BinaryOp, ProcessKind, ProcessSlots, RegionedVarAddrBase, STABLE_REGION, UnaryOp,
};
use celox_frontend_core::process::ProcessKernelBuilder;
use celox_sir::{
    BlockId, RegisterId, SIRInstruction as Inst, SIROffset, SIRTerminator as Term, SIRValue,
};
use celox_testbench::*;
use num_bigint::BigUint;

type Expr = ExprBytecode<StateLocation<Addr>>;
fn stable(var_id: Addr) -> RegionedVarAddrBase<Addr> {
    RegionedVarAddrBase {
        region: STABLE_REGION,
        var_id,
    }
}
struct Lower<'a> {
    runtime: &'a mut RuntimeProgram,
    kernel: ProcessKernelBuilder<Addr>,
    requests: Vec<KernelRequest<Addr>>,
    site: usize,
    external_observers: bool,
    settled: bool,
}
impl Lower<'_> {
    fn slot(&mut self, width: usize, string: bool) -> Addr {
        let address = self.runtime.design.allocate_testbench_slot(width, string);
        self.runtime
            .runtime_schema
            .testbench_read_roots
            .insert(address);
        address
    }
    fn imm(&mut self, width: usize, value: impl Into<BigUint>) -> RegisterId {
        let reg = self.kernel.builder().alloc_bit(width.max(1), false);
        self.kernel.builder().emit(Inst::Imm(
            reg,
            SIRValue::new(
                value.into() & ((BigUint::from(1u8) << width.max(1)) - BigUint::from(1u8)),
            ),
        ));
        reg
    }
    fn load(&mut self, address: Addr, width: usize, signed: bool) -> RegisterId {
        let reg = self.kernel.builder().alloc_bit(width.max(1), signed);
        self.kernel.builder().emit(Inst::Load(
            reg,
            stable(address),
            SIROffset::Static(0),
            width.max(1),
        ));
        reg
    }
    fn store(&mut self, address: Addr, width: usize, value: RegisterId) {
        self.kernel.builder().emit(Inst::Store(
            stable(address),
            SIROffset::Static(0),
            width.max(1),
            value,
            vec![],
            vec![],
        ));
    }
    fn cast(&mut self, value: RegisterId, width: usize, signed: bool) -> RegisterId {
        let source = self.kernel.builder().register(&value).width();
        let extend = self.kernel.builder().register(&value).is_signed() && width > source;
        let reg = self.kernel.builder().alloc_bit(width.max(1), signed);
        if width < source {
            self.kernel
                .builder()
                .emit(Inst::Slice(reg, value, 0, width.max(1)));
        } else {
            self.kernel
                .builder()
                .emit(Inst::Unary(reg, UnaryOp::Ident, value));
        }
        if extend {
            let shift = self.imm(64, (width - source) as u64);
            let reg = self.binary(BinaryOp::Shl, reg, shift, width);
            let reg = self.binary(BinaryOp::Sar, reg, shift, width);
            let typed = self.kernel.builder().alloc_bit(width.max(1), signed);
            self.kernel
                .builder()
                .emit(Inst::Unary(typed, UnaryOp::Ident, reg));
            typed
        } else {
            reg
        }
    }
    fn binary(
        &mut self,
        op: BinaryOp,
        mut l: RegisterId,
        mut r: RegisterId,
        width: usize,
    ) -> RegisterId {
        if matches!(
            op,
            BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::LtS
                | BinaryOp::LtU
                | BinaryOp::LeS
                | BinaryOp::LeU
                | BinaryOp::GtS
                | BinaryOp::GtU
                | BinaryOp::GeS
                | BinaryOp::GeU
        ) {
            let signed = matches!(
                op,
                BinaryOp::LtS | BinaryOp::LeS | BinaryOp::GtS | BinaryOp::GeS
            );
            let lw = self.kernel.builder().register(&l).width();
            let rw = self.kernel.builder().register(&r).width();
            l = self.cast(l, lw, signed);
            r = self.cast(r, rw, signed);
            l = self.cast(l, lw.max(rw), signed);
            r = self.cast(r, lw.max(rw), signed);
        }
        let reg = self.kernel.builder().alloc_bit(width.max(1), false);
        self.kernel.builder().emit(Inst::Binary(reg, l, op, r));
        reg
    }
    fn request(&mut self, request: KernelRequest<Addr>) -> Result<(), String> {
        self.settled = matches!(request, KernelRequest::Settle);
        let index = self.requests.len();
        self.requests.push(request);
        self.kernel
            .host_request(celox_design::HostRequestKind::Testbench, index as u32)
            .map_err(|e| e.to_string())
    }
    fn expression(&mut self, expr: &Expr) -> Result<lowering::Value, String> {
        let expr = expr.clone().try_map_locations(|location| {
            Ok::<_, String>(StateLocation {
                address: stable(location.address),
                byte_offset: location.byte_offset,
            })
        })?;
        let value = lowering::Expressions {
            builder: self.kernel.builder(),
        }
        .compile(&expr)?;
        let writes: Vec<_> = expr
            .ops()
            .iter()
            .filter_map(|op| {
                let address = match op {
                    ExprOpcode::StoreU64 { location, .. }
                    | ExprOpcode::StoreString { location } => location.address.var_id,
                    _ => return None,
                };
                self.writes_observable(address).then_some(address)
            })
            .collect();
        if writes.is_empty() {
            return Ok(value);
        }
        // Host requests resume through a fresh kernel entry. Preserve the
        // expression result in layout-owned state across that boundary.
        let width = self.kernel.builder().register(&value.register).width();
        let saved = self.slot(width, value.string);
        self.store(saved, width, value.register);
        for address in writes {
            let width = self.runtime.design.state_objects[&address].width;
            self.request(KernelRequest::Written { address, width })?;
        }
        Ok(lowering::Value {
            register: self.load(saved, width, false),
            string: value.string,
        })
    }
    fn writes_observable(&self, address: Addr) -> bool {
        self.external_observers || self.runtime.rtl_accesses.contains(&address)
    }
    fn observes(&self, expr: &Expr) -> bool {
        self.external_observers
            || expr.ops().iter().any(|op| match op {
                ExprOpcode::LoadU64 { location, .. }
                | ExprOpcode::LoadWide { location, .. }
                | ExprOpcode::LoadIndexed { location, .. }
                | ExprOpcode::LoadBitSelect { location, .. }
                | ExprOpcode::LoadString { location, .. } => {
                    self.runtime.rtl_accesses.contains(&location.address)
                }
                _ => false,
            })
    }
    fn settled_expression(&mut self, expr: &Expr) -> Result<lowering::Value, String> {
        // Reads within one straight-line segment see the same settled DUT.
        // A write, host service or control-flow boundary invalidates this fact.
        if self.observes(expr) && (!self.settled || self.external_observers) {
            self.request(KernelRequest::Settle)?;
        }
        self.expression(expr)
    }
    fn block(
        &mut self,
        body: &[SemanticStatement<Addr>],
        break_to: Option<BlockId>,
    ) -> Result<(), String> {
        for statement in body {
            let site = self.site;
            self.site += 1;
            if !self.kernel.builder().has_open_block() {
                let dead = self.kernel.builder().new_block();
                self.kernel.builder().switch_to_block(dead);
            }
            match statement {
                TestbenchStatement::If {
                    expr,
                    then_block,
                    else_block,
                } => {
                    let cond = self.settled_expression(expr)?.register;
                    let yes = self.kernel.builder().new_block();
                    let no = self.kernel.builder().new_block();
                    let join = self.kernel.builder().new_block();
                    self.kernel.builder().seal_block(Term::Branch {
                        cond,
                        true_block: (yes, vec![]),
                        false_block: (no, vec![]),
                    });
                    for (block, body) in [(yes, then_block), (no, else_block)] {
                        self.kernel.builder().switch_to_block(block);
                        self.settled = false;
                        self.block(body, break_to)?;
                        if self.kernel.builder().has_open_block() {
                            self.kernel.builder().seal_block(Term::Jump(join, vec![]));
                        }
                    }
                    self.kernel.builder().switch_to_block(join);
                    self.settled = false;
                }
                TestbenchStatement::Assign { dst, expr } => {
                    let expression = self.settled_expression(expr)?;
                    let value = self.cast(expression.register, dst.width, false);
                    let saved = if dst.selection.is_some() {
                        let saved = self.slot(dst.width, expression.string);
                        self.store(saved, dst.width, value);
                        Some(saved)
                    } else {
                        None
                    };
                    let offset = if let Some(selection) = &dst.selection {
                        SIROffset::Dynamic(self.expression(&selection.offset)?.register)
                    } else {
                        SIROffset::Static(0)
                    };
                    let value = saved.map_or(value, |saved| self.load(saved, dst.width, false));
                    self.kernel.builder().emit(Inst::Store(
                        stable(dst.signal.address),
                        offset,
                        dst.width,
                        value,
                        vec![],
                        vec![],
                    ));
                    if self.writes_observable(dst.signal.address) {
                        self.request(KernelRequest::Written {
                            address: dst.signal.address,
                            width: dst.signal.width,
                        })?;
                    }
                }
                TestbenchStatement::For {
                    loop_var,
                    start,
                    end,
                    inclusive,
                    step,
                    step_op,
                    reverse,
                    body,
                } => {
                    self.for_loop(
                        *loop_var, start, end, *inclusive, *step, *step_op, *reverse, body,
                        break_to,
                    )?;
                }
                TestbenchStatement::Break => {
                    if let Some(target) = break_to {
                        self.kernel.builder().seal_block(Term::Jump(target, vec![]));
                    } else {
                        self.kernel.complete();
                    }
                }
                _ => self.request(KernelRequest::Statement(site))?,
            }
        }
        Ok(())
    }
    fn bound(&mut self, bound: &LoopBound<Expr>) -> Result<(RegisterId, usize, bool), String> {
        Ok(match bound {
            LoopBound::Static(value) => {
                let width = (usize::BITS as usize - value.leading_zeros() as usize).max(32);
                let value = self.imm(width, *value as u64);
                (self.cast(value, width, true), width, true)
            }
            LoopBound::Dynamic {
                expr,
                width,
                signed,
            } => {
                let value = self.settled_expression(expr)?.register;
                (self.cast(value, *width, *signed), *width, *signed)
            }
        })
    }
    fn bound_fits(
        &mut self,
        value: RegisterId,
        width: usize,
        signed: bool,
        extra: bool,
    ) -> RegisterId {
        let source = self.kernel.builder().register(&value).clone();
        let common = source.width().max(width + 1);
        let value = self.cast(value, common, source.is_signed());
        let max = (BigUint::from(1u8) << (width - usize::from(signed))) - BigUint::from(1u8)
            + BigUint::from(u8::from(extra));
        let max = self.imm(common, max);
        let upper = self.binary(
            if source.is_signed() {
                BinaryOp::LeS
            } else {
                BinaryOp::LeU
            },
            value,
            max,
            1,
        );
        if source.is_signed() {
            let min = if signed {
                (BigUint::from(1u8) << common) - (BigUint::from(1u8) << (width - 1))
            } else {
                BigUint::ZERO
            };
            let min = self.imm(common, min);
            let lower = self.binary(BinaryOp::GeS, value, min, 1);
            self.binary(BinaryOp::And, upper, lower, 1)
        } else {
            upper
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn for_loop(
        &mut self,
        variable: Option<(SemanticSignal<Addr>, usize, bool)>,
        start: &LoopBound<Expr>,
        end: &LoopBound<Expr>,
        inclusive: bool,
        step: usize,
        step_op: Option<TestbenchOperator>,
        reverse: bool,
        body: &[SemanticStatement<Addr>],
        _outer_break: Option<BlockId>,
    ) -> Result<(), String> {
        let (first, fw, fs) = self.bound(start)?;
        // A bound can call host services: save the first value across them.
        let first_slot = self.slot(fw, false);
        self.store(first_slot, fw, first);
        let (last, lw, ls) = self.bound(end)?;
        let first = self.load(first_slot, fw, fs);
        let (width, signed) = variable.map_or((fw.max(lw).max(64), fs || ls), |(_, w, s)| (w, s));
        // A fixed-width induction variable cannot traverse an unrepresentable
        // endpoint. Diagnose that before entering an otherwise enormous loop.
        let singleton = if inclusive && fw.max(lw) > 64 {
            let equal = self.binary(BinaryOp::Eq, first, last, 1);
            let max = self.imm(fw.max(lw), u64::MAX);
            let wide = self.binary(BinaryOp::GtU, first, max, 1);
            self.binary(BinaryOp::And, equal, wide, 1)
        } else {
            self.imm(1, 0u8)
        };
        let mode_slot = self.slot(1, false);
        self.store(mode_slot, 1, singleton);
        let remaining_slot = self.slot(1, false);
        let one = self.imm(1, 1u8);
        self.store(remaining_slot, 1, one);
        if variable.is_some() {
            let first_fits = self.bound_fits(first, width, signed, false);
            let last_fits = self.bound_fits(last, width, signed, !inclusive);
            let fits = self.binary(BinaryOp::And, first_fits, last_fits, 1);
            let fits = self.binary(BinaryOp::Or, fits, singleton, 1);
            let valid = self.kernel.builder().new_block();
            let invalid = self.kernel.builder().new_block();
            self.kernel.builder().seal_block(Term::Branch {
                cond: fits,
                true_block: (valid, vec![]),
                false_block: (invalid, vec![]),
            });
            self.kernel.builder().switch_to_block(invalid);
            self.request(KernelRequest::Error(
                "non-progressing stepped for loop".into(),
            ))?;
            self.kernel.complete();
            self.kernel.builder().switch_to_block(valid);
        }
        let limit_width = if reverse { fw } else { lw };
        let limit_signed = if reverse { fs } else { ls };
        let current_slot = self.slot(width, false);
        let limit_slot = self.slot(limit_width, false);
        self.store(limit_slot, limit_width, if reverse { first } else { last });
        let current = if reverse && !inclusive {
            let one = self.imm(lw, 1u8);
            self.binary(BinaryOp::Sub, last, one, lw)
        } else if reverse {
            last
        } else {
            first
        };
        let current = self.cast(current, width, signed);
        self.store(current_slot, width, current);
        let check = self.kernel.builder().new_block();
        let body_entry = self.kernel.builder().new_block();
        let advance = self.kernel.builder().new_block();
        let exit = self.kernel.builder().new_block();
        self.kernel.builder().seal_block(Term::Jump(check, vec![]));
        self.kernel.builder().switch_to_block(check);
        let current = self.load(current_slot, width, signed);
        let limit = self.load(limit_slot, limit_width, limit_signed);
        let cmp = match (reverse, inclusive, signed && limit_signed) {
            (true, _, true) => BinaryOp::GeS,
            (true, _, false) => BinaryOp::GeU,
            (false, true, true) => BinaryOp::LeS,
            (false, true, false) => BinaryOp::LeU,
            (false, false, true) => BinaryOp::LtS,
            (false, false, false) => BinaryOp::LtU,
        };
        let cond = self.binary(cmp, current, limit, 1);
        let mode = self.load(mode_slot, 1, false);
        let remaining = self.load(remaining_slot, 1, false);
        let selected = self.kernel.builder().alloc_bit(1, false);
        self.kernel
            .builder()
            .emit(Inst::Mux(selected, mode, remaining, cond));
        let cond = selected;
        self.kernel.builder().seal_block(Term::Branch {
            cond,
            true_block: (body_entry, vec![]),
            false_block: (exit, vec![]),
        });
        self.kernel.builder().switch_to_block(body_entry);
        self.settled = false;
        if let Some((var, _, _)) = variable {
            self.store(var.address, width, current);
        }
        self.block(body, Some(exit))?;
        if self.kernel.builder().has_open_block() {
            self.kernel
                .builder()
                .seal_block(Term::Jump(advance, vec![]));
        }
        self.kernel.builder().switch_to_block(advance);
        let current = self.load(current_slot, width, signed);
        let step = self.imm(width, step as u64);
        let op = if reverse {
            BinaryOp::Sub
        } else {
            match step_op {
                Some(TestbenchOperator::Mul) => BinaryOp::Mul,
                Some(TestbenchOperator::BitOr) => BinaryOp::Or,
                Some(TestbenchOperator::BitXor) => BinaryOp::Xor,
                Some(TestbenchOperator::LogicShiftL | TestbenchOperator::ArithShiftL) => {
                    BinaryOp::Shl
                }
                _ => BinaryOp::Add,
            }
        };
        let next = self.binary(op, current, step, width);
        let next = self.cast(next, width, signed);
        let progress = self.binary(
            match (reverse, signed) {
                (true, true) => BinaryOp::LtS,
                (true, false) => BinaryOp::LtU,
                (false, true) => BinaryOp::GtS,
                (false, false) => BinaryOp::GtU,
            },
            next,
            current,
            1,
        );
        self.store(current_slot, width, next);
        let zero = self.imm(1, 0u8);
        self.store(remaining_slot, 1, zero);
        let failure = self.kernel.builder().new_block();
        self.kernel.builder().seal_block(Term::Branch {
            cond: progress,
            true_block: (check, vec![]),
            false_block: (failure, vec![]),
        });
        self.kernel.builder().switch_to_block(failure);
        self.request(KernelRequest::Error(
            "non-progressing stepped for loop".into(),
        ))?;
        self.kernel.complete();
        self.kernel.builder().switch_to_block(exit);
        self.settled = false;
        Ok(())
    }
}

pub(crate) fn lower(
    sir: &mut SirProgram,
    runtime: &mut RuntimeProgram,
    program: &mut TestbenchProgram<Addr>,
) -> Result<(), String> {
    let mut kernels = Vec::new();
    let external_observers = !program.component_bindings().is_empty()
        || !runtime.runtime_schema.persistent_comb_state.is_empty()
        || !runtime.runtime_schema.comb_observers.is_empty()
        || runtime
            .runtime_schema
            .processes
            .iter()
            .any(|p| p.kind != ProcessKind::Callable);
    for body in program.processes() {
        let slots = ProcessSlots {
            kind: ProcessKind::Callable,
            requires_timed_simulation: false,
            const_delays: false,
            program: None,
            resume: runtime.design.allocate_testbench_slot(16, false),
            status: runtime.design.allocate_testbench_slot(8, false),
            delay: runtime.design.allocate_testbench_slot(64, false),
        };
        for slot in [slots.resume, slots.status, slots.delay] {
            runtime.runtime_schema.testbench_read_roots.insert(slot);
        }
        let mut lower = Lower {
            runtime,
            kernel: ProcessKernelBuilder::new(slots),
            requests: vec![],
            site: 0,
            external_observers,
            settled: false,
        };
        lower.block(body, None)?;
        let entry = sir.processes.len();
        let mut unit = lower.kernel.build().into_map_addr(&mut |address| {
            celox_design::RegionedStateAddr::from_absolute_addr(address.region, address.var_id)
        });
        super::memory::normalize(&mut unit, &lower.runtime.design);
        unit.verify_result().map_err(|error| error.to_string())?;
        sir.processes.push(unit);
        lower.runtime.runtime_schema.processes.push(slots);
        lower.runtime.runtime_schema.process_frames.push(None);
        lower
            .runtime
            .runtime_schema
            .process_origins
            .push(Default::default());
        kernels.push(TestbenchKernel {
            entry,
            resume: slots.resume,
            status: slots.status,
            request: slots.delay,
            requests: lower.requests,
        });
    }
    program.set_kernels(kernels);
    Ok(())
}
