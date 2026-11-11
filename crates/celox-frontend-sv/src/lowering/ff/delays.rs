//! Delay controls (IEEE 1800-2023 9.4.1).

use super::*;

impl Ff<'_, '_> {
    /// `#amount` (IEEE 1800-2023 9.4.1). The amount is self-determined and
    /// read as an unsigned 64-bit time: a negative value as its two's
    /// complement, and a value with an unknown bit as zero. `#0` resumes in
    /// the inactive region. A real amount is rounded to the precision of the
    /// declaring scope, then to whole ticks of the global precision
    /// (3.14.1), as a time literal is.
    pub(super) fn delay(
        &mut self,
        amount: &sv::ir::Expr,
        ticks_per_unit: u64,
        ticks_per_precision: u64,
        precision_steps_per_tick: u64,
    ) -> Result<(), sv::AnalyzerError> {
        // Only integral constants are recorded; a real delay counts as
        // variable.
        let mut constant = None;
        let (mut delay, ticks_per_step) = match self.m.expr_float_precision(amount) {
            Some(precision) => {
                let steps = self.real_delay(
                    amount,
                    precision,
                    ticks_per_unit * precision_steps_per_tick / ticks_per_precision,
                )?;
                (
                    self.round_to_ticks(steps, precision_steps_per_tick),
                    ticks_per_precision,
                )
            }
            None => {
                let signed = self.expr_signed(amount);
                constant = self
                    .eval_const(amount)?
                    .filter(|(value, width)| {
                        !(signed && *width > 0 && value.bit(*width as u64 - 1))
                    })
                    .and_then(|(value, _)| u64::try_from(&value).ok())
                    .and_then(|value| value.checked_mul(ticks_per_unit));
                let mut arena = SLTNodeArena::new();
                let node = self.expr_slt(amount, None, &mut arena)?;
                let original = self.lower_slt(&arena, node)?;
                let node = coerce_node_width(&mut arena, node, Some(PROCESS_DELAY_WIDTH), signed)
                    .map_err(slt_error)?;
                let delay = self.lower_slt(&arena, node)?;
                (self.zero_if_unknown(delay, original), ticks_per_unit)
            }
        };
        if ticks_per_step > 1 {
            // Check before multiplying so the 64-bit delay slot cannot wrap.
            let limit = self.b.alloc_bit(PROCESS_DELAY_WIDTH, false);
            self.b.emit(SIRInstruction::Imm(
                limit,
                SIRValue::new(u64::MAX / ticks_per_step),
            ));
            let overflow = self.b.alloc_bit(1, false);
            self.b.emit(SIRInstruction::Binary(
                overflow,
                delay,
                BinaryOp::GtU,
                limit,
            ));
            let error = self.b.new_block();
            let scaled = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: overflow,
                true_block: (error, Vec::new()),
                false_block: (scaled, Vec::new()),
            });
            self.b.switch_to_block(error);
            let code = self
                .m
                .runtime_error("simulation delay overflows the simulation time range".to_string());
            self.b.seal_block(SIRTerminator::Error(code));
            self.b.switch_to_block(scaled);
            let factor = self.b.alloc_bit(PROCESS_DELAY_WIDTH, false);
            self.b
                .emit(SIRInstruction::Imm(factor, SIRValue::new(ticks_per_step)));
            let product = self.b.alloc_bit(PROCESS_DELAY_WIDTH, false);
            self.b.emit(SIRInstruction::Binary(
                product,
                delay,
                BinaryOp::Mul,
                factor,
            ));
            delay = product;
        }
        if self.kernel.is_none() {
            return self.suspend_unit_with(celox_design::ProcessStatus::InactiveDelay, delay);
        }
        self.with_kernel(|kernel| kernel.inactive_delay(delay, constant))
            .map_err(kernel_error)
    }

    /// A real delay `amount` in precision steps of the declaring scope:
    /// scaled from time units by `steps_per_unit`, rounded to the nearest
    /// step with ties away from zero (3.14.1, 6.12.1), and read as an
    /// unsigned 64-bit time like an integral delay (9.4.1). A NaN or
    /// infinite delay is zero, as an unknown integral one is.
    pub(super) fn real_delay(
        &mut self,
        amount: &sv::ir::Expr,
        precision: celox_sir::FloatPrecision,
        steps_per_unit: u64,
    ) -> Result<RegisterId, sv::AnalyzerError> {
        let scaled = if steps_per_unit > 1 {
            sv::ir::Expr::Binary {
                left: Box::new(amount.clone()),
                op: sv::ir::BinaryOp::Mul,
                right: Box::new(sv::ir::Expr::Literal(format!("{steps_per_unit}.0"))),
            }
        } else {
            amount.clone()
        };
        let precision = self.m.expr_float_precision(&scaled).unwrap_or(precision);
        let mut arena = SLTNodeArena::new();
        let node = self.expr_slt(&scaled, None, &mut arena)?;
        let node = floating::convert(
            &mut arena,
            node,
            Some(precision),
            None,
            PROCESS_DELAY_WIDTH,
            true,
            true,
        )
        .ok_or_else(|| unsupported("real delay conversion"))?;
        self.lower_slt(&arena, node)
    }

    /// `steps` precision steps in whole ticks, `steps_per_tick` steps to a
    /// tick, rounded to the nearest tick with ties up, as a time literal is.
    pub(super) fn round_to_ticks(&mut self, steps: RegisterId, steps_per_tick: u64) -> RegisterId {
        if steps_per_tick <= 1 {
            return steps;
        }
        let constant = |this: &mut Self, value: u64| {
            let register = this.b.alloc_bit(PROCESS_DELAY_WIDTH, false);
            this.b
                .emit(SIRInstruction::Imm(register, SIRValue::new(value)));
            register
        };
        let divisor = constant(self, steps_per_tick);
        let quotient = self.b.alloc_bit(PROCESS_DELAY_WIDTH, false);
        self.b.emit(SIRInstruction::Binary(
            quotient,
            steps,
            BinaryOp::DivU,
            divisor,
        ));
        let remainder = self.b.alloc_bit(PROCESS_DELAY_WIDTH, false);
        self.b.emit(SIRInstruction::Binary(
            remainder,
            steps,
            BinaryOp::RemU,
            divisor,
        ));
        // One more tick when the remainder is at least half a tick: the
        // remainder plus the rest of the tick reaches a whole one. Below
        // twice the tick, the sum cannot wrap.
        let rest = constant(self, steps_per_tick - steps_per_tick / 2);
        let sum = self.b.alloc_bit(PROCESS_DELAY_WIDTH, false);
        self.b
            .emit(SIRInstruction::Binary(sum, remainder, BinaryOp::Add, rest));
        let carry = self.b.alloc_bit(PROCESS_DELAY_WIDTH, false);
        self.b
            .emit(SIRInstruction::Binary(carry, sum, BinaryOp::DivU, divisor));
        let rounded = self.b.alloc_bit(PROCESS_DELAY_WIDTH, false);
        self.b.emit(SIRInstruction::Binary(
            rounded,
            quotient,
            BinaryOp::Add,
            carry,
        ));
        rounded
    }

    /// `value` as a two-state register that is zero when `source` has an
    /// unknown bit.
    pub(super) fn zero_if_unknown(&mut self, value: RegisterId, source: RegisterId) -> RegisterId {
        let width = self.b.register(&value).width();
        if !matches!(self.b.register(&source), RegisterType::Logic { .. }) {
            return value;
        }
        // The reduction XOR of a value is unknown exactly when a bit is.
        let reduced = self.b.alloc_logic(1);
        self.b
            .emit(SIRInstruction::Unary(reduced, UnaryOp::Xor, source));
        let zero = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        let one = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
        let is_zero = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Binary(
            is_zero,
            reduced,
            BinaryOp::EqCase,
            zero,
        ));
        let is_one = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Binary(
            is_one,
            reduced,
            BinaryOp::EqCase,
            one,
        ));
        let known = self.b.alloc_bit(1, false);
        self.b
            .emit(SIRInstruction::Binary(known, is_zero, BinaryOp::Or, is_one));
        let value = match *self.b.register(&value) {
            RegisterType::Logic { width } => {
                let two_state = self.b.alloc_bit(width, false);
                self.b
                    .emit(SIRInstruction::Unary(two_state, UnaryOp::ToTwoState, value));
                two_state
            }
            _ => value,
        };
        let zero = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        let result = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Mux(result, known, value, zero));
        result
    }
}
