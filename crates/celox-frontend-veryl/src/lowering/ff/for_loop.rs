//! `for` statements: bounds, range checks and the loop itself.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LoopBoundStatus {
    FitsLoopType,
    ExclusiveUpperSentinel,
    OutOfRange,
}

/// The widths, signedness and step of a `for` loop: fixed before any of its blocks is
/// emitted and read by every step of the lowering.
#[derive(Clone, Copy)]
struct ForLoopShape {
    loop_width: usize,
    compare_width: usize,
    math_width: usize,
    loop_signed: bool,
    comparison_signed: bool,
    inclusive: bool,
    widen_inclusive: bool,
    reverse: bool,
    step: usize,
    stepped_op: Option<Op>,
}

/// The registers and blocks of a `for` loop that its lowering steps share.
#[derive(Clone, Copy)]
struct ForLoopFrame {
    start_reg: RegisterId,
    end_reg: RegisterId,
    end_limit: RegisterId,
    init_reg: RegisterId,
    header_counter: RegisterId,
    fitcheck_counter: RegisterId,
    body_counter: RegisterId,
    empty_exit_counter: RegisterId,
    precheck_bb: Option<BlockId>,
    reverse_precheck_bb: Option<BlockId>,
    empty_exit_check_bb: Option<BlockId>,
    header_bb: BlockId,
    fitcheck_bb: BlockId,
    body_bb: BlockId,
    range_error_bb: Option<BlockId>,
    exit_bb: BlockId,
}

impl<'a> FfParser<'a> {
    pub(super) fn loop_bound_status(
        bound: &ForBound,
        width: usize,
        signed: bool,
    ) -> Option<LoopBoundStatus> {
        let value = match bound {
            ForBound::Const(v, _) => BigInt::from(*v),
            ForBound::Expression(expr) => {
                if !expr.comptime().is_const {
                    return None;
                }
                let value = expr.comptime().get_value().ok()?;
                match value {
                    Value::U64(v) => {
                        if v.signed {
                            BigInt::from(v.to_i64()?)
                        } else {
                            BigInt::from(v.to_u64()?)
                        }
                    }
                    Value::BigUint(v) => {
                        if v.signed {
                            v.to_bigint()?
                        } else {
                            BigInt::from_biguint(Sign::Plus, (*v.payload).clone())
                        }
                    }
                }
            }
        };

        if signed {
            let max = (BigInt::from(1u8) << (width.saturating_sub(1))) - BigInt::from(1u8);
            let min = -(BigInt::from(1u8) << (width.saturating_sub(1)));
            Some(if value >= min && value <= max {
                LoopBoundStatus::FitsLoopType
            } else if value == max + BigInt::from(1u8) {
                LoopBoundStatus::ExclusiveUpperSentinel
            } else {
                LoopBoundStatus::OutOfRange
            })
        } else {
            let max = (BigUint::from(1u8) << width) - BigUint::from(1u8);
            let max = BigInt::from_biguint(Sign::Plus, max);
            Some(if value.sign() != Sign::Minus && value <= max {
                LoopBoundStatus::FitsLoopType
            } else if value == max + BigInt::from(1u8) {
                LoopBoundStatus::ExclusiveUpperSentinel
            } else {
                LoopBoundStatus::OutOfRange
            })
        }
    }

    pub(super) fn loop_bound_width(&self, bound: &ForBound, signed: bool) -> Option<usize> {
        match bound {
            ForBound::Const(v, _) => {
                let value = BigInt::from(*v);
                Some(if signed {
                    if value.sign() == Sign::Minus {
                        let magnitude = (-value - BigInt::from(1u8)).to_biguint()?;
                        magnitude.bits() as usize + 1
                    } else {
                        value.to_biguint()?.bits() as usize + 1
                    }
                } else {
                    let magnitude = value.to_biguint()?;
                    (magnitude.bits() as usize).max(1)
                })
            }
            ForBound::Expression(expr) => {
                let comptime_width = expr.comptime().r#type.total_width();
                let context_width = Some(expr.comptime().expr_context.width).filter(|w| *w > 0);
                Some(
                    comptime_width
                        .into_iter()
                        .chain(context_width)
                        .chain(std::iter::once(self.get_expression_width(expr)))
                        .max()
                        .unwrap_or(1),
                )
            }
        }
    }

    pub(super) fn step_math_width(base_width: usize, stepped_op: Option<Op>, step: usize) -> usize {
        match stepped_op {
            Some(Op::Mul) => {
                let step_bits = (usize::BITS as usize - step.leading_zeros() as usize).max(1);
                base_width.saturating_add(step_bits)
            }
            Some(Op::LogicShiftL | Op::ArithShiftL) => base_width.saturating_add(step.max(1)),
            Some(Op::BitOr | Op::BitXor) => base_width,
            Some(Op::Add) | None => {
                if step <= 1 {
                    return base_width;
                }
                let step_bits = (usize::BITS as usize - step.leading_zeros() as usize).max(1);
                base_width.saturating_add(step_bits)
            }
            Some(_) => base_width,
        }
    }

    pub(super) fn truncate_usize_to_width(value: usize, width: usize) -> usize {
        if width >= usize::BITS as usize {
            value
        } else if width == 0 {
            0
        } else {
            value & ((1usize << width) - 1)
        }
    }

    pub(super) fn emit_loop_value_fits<A>(
        ir_builder: &mut SIRBuilder<A>,
        value_reg: RegisterId,
        compare_width: usize,
        loop_width: usize,
        loop_signed: bool,
        allow_exclusive_upper_sentinel: bool,
    ) -> RegisterId {
        debug_assert!(compare_width >= loop_width);
        let one = BigUint::from(1u8);
        if loop_signed {
            let min_payload = (&one << compare_width) - (&one << (loop_width - 1));
            let max_payload = (&one << (loop_width - 1)) - &one;

            let min_reg = ir_builder.alloc_bit(compare_width, true);
            ir_builder.emit(SIRInstruction::Imm(min_reg, SIRValue::new(min_payload)));
            let max_reg = ir_builder.alloc_bit(compare_width, true);
            ir_builder.emit(SIRInstruction::Imm(
                max_reg,
                SIRValue::new(max_payload.clone()),
            ));

            let ge_min = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                ge_min,
                value_reg,
                BinaryOp::GeS,
                min_reg,
            ));
            let le_max = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                le_max,
                value_reg,
                BinaryOp::LeS,
                max_reg,
            ));
            let fits_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                fits_reg,
                ge_min,
                BinaryOp::LogicAnd,
                le_max,
            ));

            if allow_exclusive_upper_sentinel {
                let sentinel_reg = ir_builder.alloc_bit(compare_width, true);
                ir_builder.emit(SIRInstruction::Imm(
                    sentinel_reg,
                    SIRValue::new(max_payload + &one),
                ));
                let is_sentinel = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    is_sentinel,
                    value_reg,
                    BinaryOp::Eq,
                    sentinel_reg,
                ));
                let allowed_reg = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    allowed_reg,
                    fits_reg,
                    BinaryOp::LogicOr,
                    is_sentinel,
                ));
                allowed_reg
            } else {
                fits_reg
            }
        } else {
            let max_payload = (&one << loop_width) - &one;
            let max_reg = ir_builder.alloc_bit(compare_width, false);
            ir_builder.emit(SIRInstruction::Imm(
                max_reg,
                SIRValue::new(max_payload.clone()),
            ));
            let fits_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                fits_reg,
                value_reg,
                BinaryOp::LeU,
                max_reg,
            ));

            if allow_exclusive_upper_sentinel {
                let sentinel_reg = ir_builder.alloc_bit(compare_width, false);
                ir_builder.emit(SIRInstruction::Imm(
                    sentinel_reg,
                    SIRValue::new(max_payload + &one),
                ));
                let is_sentinel = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    is_sentinel,
                    value_reg,
                    BinaryOp::Eq,
                    sentinel_reg,
                ));
                let allowed_reg = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    allowed_reg,
                    fits_reg,
                    BinaryOp::LogicOr,
                    is_sentinel,
                ));
                allowed_reg
            } else {
                fits_reg
            }
        }
    }

    pub(super) fn parse_for_bound<A>(
        &mut self,
        bound: &ForBound,
        canonical_width: usize,
        width: usize,
        signed: bool,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<RegisterId, ParserError> {
        match bound {
            ForBound::Const(v, _) => {
                let reg = ir_builder.alloc_bit(width, signed);
                ir_builder.emit(SIRInstruction::Imm(reg, SIRValue::new(*v as u64)));
                Ok(reg)
            }
            ForBound::Expression(expr) => {
                self.parse_expression(expr, targets, domain, convert, sources, ir_builder, None)?;
                let reg = self.stack.pop_back().unwrap();
                let source_signed = expr.comptime().r#type.signed;
                let extend_signed = source_signed && signed;
                let canonical =
                    self.cast_reg_width_ext(ir_builder, reg, canonical_width, extend_signed);
                let canonical = match ir_builder.register(&canonical) {
                    RegisterType::Bit {
                        width: reg_width,
                        signed: reg_signed,
                    } if *reg_width == canonical_width && *reg_signed == signed => canonical,
                    _ => {
                        let bit_reg = ir_builder.alloc_bit(canonical_width, signed);
                        ir_builder.emit(SIRInstruction::Unary(bit_reg, UnaryOp::Ident, canonical));
                        bit_reg
                    }
                };
                let widened = self.cast_reg_width_ext(ir_builder, canonical, width, signed);
                match ir_builder.register(&widened) {
                    RegisterType::Bit {
                        width: reg_width,
                        signed: reg_signed,
                    } if *reg_width == width && *reg_signed == signed => Ok(widened),
                    _ => {
                        let bit_reg = ir_builder.alloc_bit(width, signed);
                        ir_builder.emit(SIRInstruction::Unary(bit_reg, UnaryOp::Ident, widened));
                        Ok(bit_reg)
                    }
                }
            }
        }
    }

    /// The bounds, the direction and the step of a `for` range, and the values of its
    /// constant bounds.
    fn for_range_parts(
        range: &ForRange,
    ) -> (
        &ForBound,
        &ForBound,
        bool,
        usize,
        bool,
        Option<Op>,
        Option<usize>,
        Option<usize>,
    ) {
        match range {
            ForRange::Forward {
                start,
                end,
                inclusive,
                step,
            } => (
                start,
                end,
                *inclusive,
                *step,
                false,
                None,
                Self::bound_const_value(start),
                Self::bound_const_value(end),
            ),
            ForRange::Reverse {
                start,
                end,
                inclusive,
                step,
            } => (
                start,
                end,
                *inclusive,
                *step,
                true,
                None,
                Self::bound_const_value(start),
                Self::bound_const_value(end),
            ),
            ForRange::Stepped {
                start,
                end,
                inclusive,
                step,
                op,
            } => (
                start,
                end,
                *inclusive,
                *step,
                false,
                Some(*op),
                Self::bound_const_value(start),
                Self::bound_const_value(end),
            ),
        }
    }

    /// The block that checks the end bound of a forward loop whose counter is wider than
    /// the loop variable, and decides whether the loop runs at all.
    fn emit_for_forward_precheck<A>(
        &self,
        shape: &ForLoopShape,
        frame: &ForLoopFrame,
        ir_builder: &mut SIRBuilder<A>,
    ) {
        let ForLoopShape {
            loop_width,
            compare_width,
            loop_signed,
            comparison_signed,
            inclusive,
            widen_inclusive,
            ..
        } = *shape;
        let ForLoopFrame {
            start_reg,
            end_reg,
            end_limit,
            precheck_bb,
            empty_exit_check_bb,
            fitcheck_bb,
            range_error_bb,
            ..
        } = *frame;
        if let (Some(precheck_bb), Some(empty_exit_check_bb), Some(range_error_bb)) =
            (precheck_bb, empty_exit_check_bb, range_error_bb)
        {
            ir_builder.switch_to_block(precheck_bb);
            let end_allowed_reg = if compare_width > 64 {
                Self::emit_loop_value_fits(
                    ir_builder,
                    end_reg,
                    compare_width,
                    loop_width,
                    loop_signed,
                    !inclusive,
                )
            } else {
                let end_visible =
                    self.cast_reg_width_ext(ir_builder, end_reg, loop_width, loop_signed);
                let end_roundtrip =
                    self.cast_reg_width_ext(ir_builder, end_visible, compare_width, loop_signed);
                let end_fits_reg = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    end_fits_reg,
                    end_reg,
                    BinaryOp::Eq,
                    end_roundtrip,
                ));
                if !inclusive {
                    let sentinel_reg = ir_builder.alloc_bit(compare_width, loop_signed);
                    let sentinel_value = if loop_signed {
                        1u64 << (loop_width - 1)
                    } else {
                        1u64 << loop_width
                    };
                    ir_builder.emit(SIRInstruction::Imm(
                        sentinel_reg,
                        SIRValue::new(sentinel_value),
                    ));
                    let end_is_sentinel_reg = ir_builder.alloc_bit(1, false);
                    ir_builder.emit(SIRInstruction::Binary(
                        end_is_sentinel_reg,
                        end_reg,
                        BinaryOp::Eq,
                        sentinel_reg,
                    ));
                    let allowed_reg = ir_builder.alloc_bit(1, false);
                    ir_builder.emit(SIRInstruction::Binary(
                        allowed_reg,
                        end_fits_reg,
                        BinaryOp::LogicOr,
                        end_is_sentinel_reg,
                    ));
                    allowed_reg
                } else {
                    end_fits_reg
                }
            };
            let precheck_pass_bb = ir_builder.new_block();
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: end_allowed_reg,
                true_block: (precheck_pass_bb, vec![]),
                false_block: (range_error_bb, vec![]),
            });
            ir_builder.switch_to_block(precheck_pass_bb);
            let cond_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                cond_reg,
                start_reg,
                if comparison_signed {
                    if inclusive {
                        BinaryOp::LeS
                    } else {
                        BinaryOp::LtS
                    }
                } else if inclusive && !widen_inclusive {
                    BinaryOp::LeU
                } else {
                    BinaryOp::LtU
                },
                end_limit,
            ));
            // Keep the initial widened-range decision in the precheck block so
            // we do not reuse the same block-param SSA value across multiple blocks.
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: cond_reg,
                true_block: (fitcheck_bb, vec![start_reg]),
                false_block: (empty_exit_check_bb, vec![start_reg]),
            });
        }
    }

    /// The block that checks both bounds of a reverse loop whose counter is wider than the
    /// loop variable.
    fn emit_for_reverse_precheck<A>(
        shape: &ForLoopShape,
        frame: &ForLoopFrame,
        ir_builder: &mut SIRBuilder<A>,
    ) {
        let ForLoopShape {
            loop_width,
            compare_width,
            loop_signed,
            inclusive,
            ..
        } = *shape;
        let ForLoopFrame {
            start_reg,
            end_reg,
            init_reg,
            reverse_precheck_bb,
            header_bb,
            range_error_bb,
            ..
        } = *frame;
        if let (Some(reverse_precheck_bb), Some(range_error_bb)) =
            (reverse_precheck_bb, range_error_bb)
        {
            ir_builder.switch_to_block(reverse_precheck_bb);
            let start_fits = Self::emit_loop_value_fits(
                ir_builder,
                start_reg,
                compare_width,
                loop_width,
                loop_signed,
                false,
            );
            let end_fits = Self::emit_loop_value_fits(
                ir_builder,
                end_reg,
                compare_width,
                loop_width,
                loop_signed,
                !inclusive,
            );
            let bounds_fit = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                bounds_fit,
                start_fits,
                BinaryOp::LogicAnd,
                end_fits,
            ));
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: bounds_fit,
                true_block: (header_bb, vec![init_reg]),
                false_block: (range_error_bb, vec![]),
            });
        }
    }

    /// The loop header: compare the counter with the continuation bound.
    fn emit_for_header<A>(
        shape: &ForLoopShape,
        frame: &ForLoopFrame,
        ir_builder: &mut SIRBuilder<A>,
    ) {
        let ForLoopShape {
            comparison_signed,
            inclusive,
            widen_inclusive,
            reverse,
            ..
        } = *shape;
        let ForLoopFrame {
            start_reg,
            end_limit,
            header_counter,
            empty_exit_check_bb,
            header_bb,
            fitcheck_bb,
            exit_bb,
            ..
        } = *frame;
        ir_builder.switch_to_block(header_bb);
        if reverse {
            let in_range = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                in_range,
                header_counter,
                if comparison_signed {
                    BinaryOp::GeS
                } else {
                    BinaryOp::GeU
                },
                start_reg,
            ));
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: in_range,
                true_block: (fitcheck_bb, vec![header_counter]),
                false_block: empty_exit_check_bb
                    .map_or((exit_bb, vec![]), |block| (block, vec![header_counter])),
            });
        } else {
            let cond_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                cond_reg,
                header_counter,
                if comparison_signed {
                    if inclusive {
                        BinaryOp::LeS
                    } else {
                        BinaryOp::LtS
                    }
                } else if inclusive && !widen_inclusive {
                    BinaryOp::LeU
                } else {
                    BinaryOp::LtU
                },
                end_limit,
            ));
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: cond_reg,
                true_block: (fitcheck_bb, vec![header_counter]),
                false_block: (exit_bb, vec![]),
            });
        }
    }

    /// The block that checks, when the loop ends without another iteration, that the
    /// counter and the start bound fit the loop variable.
    fn emit_for_empty_exit_check<A>(
        &self,
        shape: &ForLoopShape,
        frame: &ForLoopFrame,
        ir_builder: &mut SIRBuilder<A>,
    ) {
        let ForLoopShape {
            loop_width,
            compare_width,
            loop_signed,
            ..
        } = *shape;
        let ForLoopFrame {
            start_reg,
            empty_exit_counter,
            empty_exit_check_bb,
            range_error_bb,
            exit_bb,
            ..
        } = *frame;
        if let (Some(empty_exit_check_bb), Some(range_error_bb)) =
            (empty_exit_check_bb, range_error_bb)
        {
            ir_builder.switch_to_block(empty_exit_check_bb);
            let empty_fits_reg = if compare_width > 64 {
                Self::emit_loop_value_fits(
                    ir_builder,
                    empty_exit_counter,
                    compare_width,
                    loop_width,
                    loop_signed,
                    false,
                )
            } else {
                let empty_visible = self.cast_reg_width_ext(
                    ir_builder,
                    empty_exit_counter,
                    loop_width,
                    loop_signed,
                );
                let empty_roundtrip =
                    self.cast_reg_width_ext(ir_builder, empty_visible, compare_width, loop_signed);
                let empty_fits_reg = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    empty_fits_reg,
                    empty_exit_counter,
                    BinaryOp::Eq,
                    empty_roundtrip,
                ));
                empty_fits_reg
            };
            let empty_start_fits_reg = if compare_width > 64 {
                Self::emit_loop_value_fits(
                    ir_builder,
                    start_reg,
                    compare_width,
                    loop_width,
                    loop_signed,
                    false,
                )
            } else {
                let empty_start_visible =
                    self.cast_reg_width_ext(ir_builder, start_reg, loop_width, loop_signed);
                let empty_start_roundtrip = self.cast_reg_width_ext(
                    ir_builder,
                    empty_start_visible,
                    compare_width,
                    loop_signed,
                );
                let empty_start_fits_reg = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    empty_start_fits_reg,
                    start_reg,
                    BinaryOp::Eq,
                    empty_start_roundtrip,
                ));
                empty_start_fits_reg
            };
            let empty_allowed_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                empty_allowed_reg,
                empty_fits_reg,
                BinaryOp::LogicAnd,
                empty_start_fits_reg,
            ));
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: empty_allowed_reg,
                true_block: (exit_bb, vec![]),
                false_block: (range_error_bb, vec![]),
            });
        }
    }

    /// The block that publishes the loop variable and checks that the counter fits it, and
    /// the block of the range error.
    #[allow(clippy::too_many_arguments)]
    fn emit_for_fitcheck<A>(
        &mut self,
        stmt: &ForStatement,
        shape: &ForLoopShape,
        frame: &ForLoopFrame,
        range_message: String,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        ir_builder: &mut SIRBuilder<A>,
    ) {
        let ForLoopShape {
            loop_width,
            compare_width,
            loop_signed,
            ..
        } = *shape;
        let ForLoopFrame {
            fitcheck_counter,
            fitcheck_bb,
            body_bb,
            range_error_bb,
            ..
        } = *frame;
        ir_builder.switch_to_block(fitcheck_bb);
        let fitcheck_visible_reg =
            self.cast_reg_width_ext(ir_builder, fitcheck_counter, loop_width, loop_signed);
        // Publish the loop variable before entering the body block so the
        // body itself stays a single widened block for native codegen.
        ir_builder.emit(SIRInstruction::Store(
            convert(stmt.var_id, domain.region()),
            SIROffset::Static(0),
            loop_width,
            fitcheck_visible_reg,
            Vec::new(),
            Vec::new(),
        ));
        if let Some(range_error_bb) = range_error_bb {
            let fits_loop_reg = if compare_width > 64 {
                Self::emit_loop_value_fits(
                    ir_builder,
                    fitcheck_counter,
                    compare_width,
                    loop_width,
                    loop_signed,
                    false,
                )
            } else {
                let visible_roundtrip = self.cast_reg_width_ext(
                    ir_builder,
                    fitcheck_visible_reg,
                    compare_width,
                    loop_signed,
                );
                let fits_loop_reg = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    fits_loop_reg,
                    fitcheck_counter,
                    BinaryOp::Eq,
                    visible_roundtrip,
                ));
                fits_loop_reg
            };
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: fits_loop_reg,
                true_block: (body_bb, vec![fitcheck_counter]),
                false_block: (range_error_bb, vec![]),
            });
        } else {
            ir_builder.seal_block(SIRTerminator::Jump(body_bb, vec![fitcheck_counter]));
        }
        if let Some(range_error_bb) = range_error_bb {
            ir_builder.switch_to_block(range_error_bb);
            let error_code = self.runtime_error(range_message, vec![stmt.var_id]);
            ir_builder.seal_block(SIRTerminator::Error(error_code));
        }
    }

    /// The step of a forward loop after its body: compute the next counter, stop with a
    /// runtime error when it does not progress, and continue or leave the loop.
    fn emit_for_forward_step<A>(
        &mut self,
        stmt: &ForStatement,
        shape: &ForLoopShape,
        frame: &ForLoopFrame,
        non_progress_message: String,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let ForLoopShape {
            loop_width,
            compare_width,
            math_width,
            loop_signed,
            comparison_signed,
            inclusive,
            widen_inclusive,
            step,
            stepped_op,
            ..
        } = *shape;
        let ForLoopFrame {
            end_limit,
            body_counter,
            header_bb,
            exit_bb,
            ..
        } = *frame;
        let step_width = if matches!(stepped_op, Some(Op::BitOr | Op::BitXor)) {
            loop_width
        } else {
            math_width
        };
        let current_step =
            self.cast_reg_width_ext(ir_builder, body_counter, step_width, loop_signed);
        let step_reg = ir_builder.alloc_bit(step_width, loop_signed);
        let step_value = Self::truncate_usize_to_width(step, step_width);
        ir_builder.emit(SIRInstruction::Imm(
            step_reg,
            SIRValue::new(step_value as u64),
        ));
        let next_step = ir_builder.alloc_bit(step_width, loop_signed);
        let op = match stepped_op {
            Some(Op::Mul) => BinaryOp::Mul,
            Some(Op::LogicShiftL | Op::ArithShiftL) => BinaryOp::Shl,
            Some(Op::BitOr) => BinaryOp::Or,
            Some(Op::BitXor) => BinaryOp::Xor,
            Some(Op::Add) | None => BinaryOp::Add,
            Some(other) => {
                self.local_working_vars.remove(&stmt.var_id);
                return Err(ParserError::illegal_context(
                    "for loop step operator in always_ff",
                    format!("{other:?}"),
                    Some(&stmt.token),
                ));
            }
        };
        ir_builder.emit(SIRInstruction::Binary(
            next_step,
            current_step,
            op,
            step_reg,
        ));
        let current_math =
            self.cast_reg_width_ext(ir_builder, current_step, math_width, loop_signed);
        // Compound assignment updates the fixed-width loop variable
        // before the next comparison. Truncate to that visible width so
        // overflow cannot be hidden by the widened comparison path.
        let next_visible = self.cast_reg_width_ext(ir_builder, next_step, loop_width, loop_signed);
        let next_reg = self.cast_reg_width_ext(ir_builder, next_visible, math_width, loop_signed);
        let progress_reg = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Binary(
            progress_reg,
            next_reg,
            BinaryOp::Ne,
            current_math,
        ));
        let stall_bb = ir_builder.new_block();
        let continue_bb = ir_builder.new_block();
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: progress_reg,
            true_block: (continue_bb, vec![]),
            false_block: (stall_bb, vec![]),
        });
        ir_builder.switch_to_block(continue_bb);
        let increasing_reg = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Binary(
            increasing_reg,
            next_reg,
            if loop_signed {
                BinaryOp::GtS
            } else {
                BinaryOp::GtU
            },
            current_math,
        ));
        let range_check_bb = ir_builder.new_block();
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: increasing_reg,
            true_block: (range_check_bb, vec![]),
            false_block: (stall_bb, vec![]),
        });
        ir_builder.switch_to_block(range_check_bb);
        let end_reg = self.cast_reg_width_ext(ir_builder, end_limit, math_width, loop_signed);
        let in_range_reg = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Binary(
            in_range_reg,
            next_reg,
            if comparison_signed {
                if inclusive {
                    BinaryOp::LeS
                } else {
                    BinaryOp::LtS
                }
            } else if inclusive && !widen_inclusive {
                BinaryOp::LeU
            } else {
                BinaryOp::LtU
            },
            end_reg,
        ));
        let next_counter =
            self.cast_reg_width_ext(ir_builder, next_reg, compare_width, loop_signed);
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: in_range_reg,
            true_block: (header_bb, vec![next_counter]),
            false_block: (exit_bb, vec![]),
        });
        ir_builder.switch_to_block(stall_bb);
        let error_code = self.runtime_error(non_progress_message, vec![stmt.var_id]);
        ir_builder.seal_block(SIRTerminator::Error(error_code));
        Ok(())
    }

    /// The step of a reverse loop after its body.
    fn emit_for_reverse_step<A>(
        &mut self,
        stmt: &ForStatement,
        shape: &ForLoopShape,
        frame: &ForLoopFrame,
        non_progress_message: String,
        ir_builder: &mut SIRBuilder<A>,
    ) {
        let ForLoopShape {
            loop_width,
            compare_width,
            math_width,
            loop_signed,
            comparison_signed,
            step,
            ..
        } = *shape;
        let ForLoopFrame {
            start_reg,
            body_counter,
            header_bb,
            exit_bb,
            ..
        } = *frame;
        let current_math =
            self.cast_reg_width_ext(ir_builder, body_counter, math_width, loop_signed);
        let start_math = self.cast_reg_width_ext(ir_builder, start_reg, math_width, loop_signed);
        let step_reg = ir_builder.alloc_bit(math_width, loop_signed);
        ir_builder.emit(SIRInstruction::Imm(step_reg, SIRValue::new(step as u64)));
        let next_raw = ir_builder.alloc_bit(math_width, loop_signed);
        ir_builder.emit(SIRInstruction::Binary(
            next_raw,
            current_math,
            BinaryOp::Sub,
            step_reg,
        ));
        let next_visible = self.cast_reg_width_ext(ir_builder, next_raw, loop_width, loop_signed);
        let next_reg = self.cast_reg_width_ext(ir_builder, next_visible, math_width, loop_signed);
        let decreasing_reg = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Binary(
            decreasing_reg,
            next_reg,
            if loop_signed {
                BinaryOp::LtS
            } else {
                BinaryOp::LtU
            },
            current_math,
        ));
        let range_check_bb = ir_builder.new_block();
        let stall_bb = ir_builder.new_block();
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: decreasing_reg,
            true_block: (range_check_bb, vec![]),
            false_block: (stall_bb, vec![]),
        });
        ir_builder.switch_to_block(range_check_bb);
        let in_range_reg = ir_builder.alloc_bit(1, false);
        ir_builder.emit(SIRInstruction::Binary(
            in_range_reg,
            next_reg,
            if comparison_signed {
                BinaryOp::GeS
            } else {
                BinaryOp::GeU
            },
            start_math,
        ));
        let next_counter =
            self.cast_reg_width_ext(ir_builder, next_reg, compare_width, loop_signed);
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: in_range_reg,
            true_block: (header_bb, vec![next_counter]),
            false_block: (exit_bb, vec![]),
        });
        ir_builder.switch_to_block(stall_bb);
        let error_code = self.runtime_error(non_progress_message, vec![stmt.var_id]);
        ir_builder.seal_block(SIRTerminator::Error(error_code));
    }

    pub(super) fn parse_for_statement<A>(
        &mut self,
        stmt: &ForStatement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let base_loop_width =
            resolve_total_width(self.module, &self.module.variables[&stmt.var_id])?;
        let loop_signed = stmt.var_type.signed;

        let (start_bound, end_bound, inclusive, step, reverse, stepped_op, start_const, end_const) =
            Self::for_range_parts(&stmt.range);
        // The emitted int counter compares against the continuation bound's
        // type, which may be unsigned even when the counter is signed.
        let continuation_bound = if reverse { start_bound } else { end_bound };
        let comparison_signed = loop_signed
            && match continuation_bound {
                ForBound::Const(_, signed) => *signed,
                ForBound::Expression(expr) => crate::context_width::expression_signed(expr),
            };
        let loop_var_name = veryl_parser::resource_table::get_str_value(stmt.var_name)
            .unwrap_or_else(|| "<unknown>".to_string());
        let non_progress_message =
            format!("Non-progressing for loop in always_ff (loop variable `{loop_var_name}`)");
        let range_message = format!(
            "For loop value exceeds loop variable range in always_ff (loop variable `{loop_var_name}`)"
        );
        let loop_width = base_loop_width.max(1);

        let const_empty = Self::const_range_is_empty(reverse, start_const, end_const, inclusive);
        let const_singleton =
            Self::const_range_is_singleton(reverse, start_const, end_const, inclusive);
        if start_const.is_some()
            && end_const.is_some()
            && Self::step_can_stall(reverse, stepped_op, step, start_const, loop_width)
            && !const_empty
            && !const_singleton
        {
            return Err(ParserError::illegal_context(
                "non-progressing for loop in always_ff",
                format!("{:?}", stmt.var_name),
                Some(&stmt.token),
            ));
        }

        let start_bound_width = self
            .loop_bound_width(start_bound, loop_signed)
            .unwrap_or(loop_width);
        let end_bound_width = self
            .loop_bound_width(end_bound, loop_signed)
            .unwrap_or(loop_width);
        let start_status = Self::loop_bound_status(start_bound, loop_width, loop_signed);
        let end_status = Self::loop_bound_status(end_bound, loop_width, loop_signed);
        let uses_exclusive_end_sentinel = !inclusive;
        // Veryl now models loop variables as i32. Reject statically invalid
        // bounds, but keep supporting the exclusive upper sentinel used for
        // full-range iteration such as `0..256` on an 8-bit loop variable.
        if matches!(
            start_status,
            Some(LoopBoundStatus::OutOfRange | LoopBoundStatus::ExclusiveUpperSentinel)
        ) || matches!(end_status, Some(LoopBoundStatus::OutOfRange))
            || (inclusive && end_status == Some(LoopBoundStatus::ExclusiveUpperSentinel))
        {
            return Err(ParserError::illegal_context(
                "for loop bound exceeding i32 loop variable",
                format!("{:?}", stmt.var_name),
                Some(&stmt.token),
            ));
        }
        let counter_width = loop_width.max(1);
        let bound_width = counter_width.max(start_bound_width).max(end_bound_width);
        let widen_inclusive = inclusive && !loop_signed;
        let compare_width = if widen_inclusive {
            bound_width.saturating_add(1)
        } else if uses_exclusive_end_sentinel {
            bound_width.max(counter_width.saturating_add(1))
        } else {
            bound_width
        };
        let math_width = if reverse {
            Self::step_math_width(compare_width, Some(Op::Add), step)
        } else {
            Self::step_math_width(compare_width, stepped_op, step)
        };

        let start_reg = self.parse_for_bound(
            start_bound,
            compare_width,
            compare_width,
            loop_signed,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        let end_reg = self.parse_for_bound(
            end_bound,
            compare_width,
            compare_width,
            loop_signed,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;

        let one_reg = ir_builder.alloc_bit(compare_width, loop_signed);
        ir_builder.emit(SIRInstruction::Imm(one_reg, SIRValue::new(1u64)));
        let end_limit = if widen_inclusive {
            let reg = ir_builder.alloc_bit(compare_width, loop_signed);
            ir_builder.emit(SIRInstruction::Binary(reg, end_reg, BinaryOp::Add, one_reg));
            reg
        } else {
            end_reg
        };

        let init_reg = if reverse && !inclusive {
            let raw = ir_builder.alloc_bit(compare_width, loop_signed);
            ir_builder.emit(SIRInstruction::Binary(raw, end_reg, BinaryOp::Sub, one_reg));
            let visible = self.cast_reg_width_ext(ir_builder, raw, loop_width, loop_signed);
            self.cast_reg_width_ext(ir_builder, visible, compare_width, loop_signed)
        } else if reverse {
            end_reg
        } else {
            start_reg
        };

        let header_counter = ir_builder.alloc_bit(compare_width, loop_signed);
        let fitcheck_counter = ir_builder.alloc_bit(compare_width, loop_signed);
        let body_counter = ir_builder.alloc_bit(compare_width, loop_signed);
        let needs_range_check = compare_width != loop_width;
        let precheck_bb = (!reverse && needs_range_check).then(|| ir_builder.new_block());
        let reverse_precheck_bb = (reverse && needs_range_check).then(|| ir_builder.new_block());
        let empty_exit_counter = ir_builder.alloc_bit(compare_width, loop_signed);
        let empty_exit_check_bb =
            needs_range_check.then(|| ir_builder.new_block_with(vec![empty_exit_counter]));
        let header_bb = ir_builder.new_block_with(vec![header_counter]);
        let fitcheck_bb = ir_builder.new_block_with(vec![fitcheck_counter]);
        let body_bb = ir_builder.new_block_with(vec![body_counter]);
        let range_error_bb = needs_range_check.then(|| ir_builder.new_block());
        let exit_bb = ir_builder.new_block();
        if let Some(precheck_bb) = precheck_bb {
            ir_builder.seal_block(SIRTerminator::Jump(precheck_bb, vec![]));
        } else if let Some(reverse_precheck_bb) = reverse_precheck_bb {
            ir_builder.seal_block(SIRTerminator::Jump(reverse_precheck_bb, vec![]));
        } else {
            ir_builder.seal_block(SIRTerminator::Jump(header_bb, vec![init_reg]));
        }

        let shape = ForLoopShape {
            loop_width,
            compare_width,
            math_width,
            loop_signed,
            comparison_signed,
            inclusive,
            widen_inclusive,
            reverse,
            step,
            stepped_op,
        };
        let frame = ForLoopFrame {
            start_reg,
            end_reg,
            end_limit,
            init_reg,
            header_counter,
            fitcheck_counter,
            body_counter,
            empty_exit_counter,
            precheck_bb,
            reverse_precheck_bb,
            empty_exit_check_bb,
            header_bb,
            fitcheck_bb,
            body_bb,
            range_error_bb,
            exit_bb,
        };

        let pre_loop_defined = self.defined.defined_ranges.clone();
        let pre_loop_dynamic = self.defined.dynamic_defined_vars.clone();

        self.emit_for_forward_precheck(&shape, &frame, ir_builder);
        Self::emit_for_reverse_precheck(&shape, &frame, ir_builder);
        Self::emit_for_header(&shape, &frame, ir_builder);
        self.emit_for_empty_exit_check(&shape, &frame, ir_builder);
        self.emit_for_fitcheck(
            stmt,
            &shape,
            &frame,
            range_message,
            domain,
            convert,
            ir_builder,
        );
        ir_builder.switch_to_block(body_bb);
        self.local_working_vars.insert(stmt.var_id);

        let mut local_defined = self.defined.defined_ranges.clone();
        local_defined.insert(stmt.var_id, (0..loop_width).collect());
        let prev_defined = std::mem::replace(&mut self.defined.defined_ranges, local_defined);
        let mut local_dynamic = self.defined.dynamic_defined_vars.clone();
        local_dynamic.insert(stmt.var_id);
        let prev_dynamic = std::mem::replace(&mut self.defined.dynamic_defined_vars, local_dynamic);

        self.loop_exit_blocks.push(exit_bb);
        let body_flow =
            self.parse_statement_list(&stmt.body, targets, domain, convert, sources, ir_builder)?;
        self.loop_exit_blocks.pop();

        self.defined.defined_ranges = prev_defined;
        self.defined.dynamic_defined_vars = prev_dynamic;

        if matches!(body_flow, ControlFlow::Break) {
            self.local_working_vars.remove(&stmt.var_id);
            ir_builder.switch_to_block(exit_bb);
            self.defined.defined_ranges = pre_loop_defined;
            self.defined.dynamic_defined_vars = pre_loop_dynamic;
            return Ok(());
        }

        if !reverse {
            self.emit_for_forward_step(stmt, &shape, &frame, non_progress_message, ir_builder)?;
        } else {
            self.emit_for_reverse_step(stmt, &shape, &frame, non_progress_message, ir_builder);
        }

        self.local_working_vars.remove(&stmt.var_id);
        ir_builder.switch_to_block(exit_bb);
        self.defined.defined_ranges = pre_loop_defined;
        self.defined.dynamic_defined_vars = pre_loop_dynamic;
        Ok(())
    }

    pub(super) fn bound_const_value(bound: &ForBound) -> Option<usize> {
        match bound {
            ForBound::Const(v, _) => Some(*v),
            ForBound::Expression(expr) => eval_constexpr(expr)?.to_usize(),
        }
    }

    pub(super) fn step_can_stall(
        reverse: bool,
        stepped_op: Option<Op>,
        step: usize,
        start_const: Option<usize>,
        loop_width: usize,
    ) -> bool {
        if reverse {
            return step == 0;
        }
        let bitwise_step = Self::truncate_usize_to_width(step, loop_width);
        match stepped_op {
            Some(Op::Mul) => step == 0 || step == 1 || start_const == Some(0),
            Some(Op::LogicShiftL | Op::ArithShiftL) => step == 0 || start_const == Some(0),
            Some(Op::BitOr) => start_const
                .map(|start| Self::truncate_usize_to_width(start, loop_width))
                .is_some_and(|start| (start | bitwise_step) == start),
            Some(Op::BitXor) => bitwise_step == 0,
            Some(Op::Add) | None => step == 0,
            Some(_) => false,
        }
    }

    pub(super) fn const_range_is_empty(
        reverse: bool,
        start_const: Option<usize>,
        end_const: Option<usize>,
        inclusive: bool,
    ) -> bool {
        let (Some(start), Some(end)) = (start_const, end_const) else {
            return false;
        };
        if reverse {
            if inclusive { end < start } else { end <= start }
        } else if inclusive {
            start > end
        } else {
            start >= end
        }
    }

    pub(super) fn const_range_is_singleton(
        _reverse: bool,
        start_const: Option<usize>,
        end_const: Option<usize>,
        inclusive: bool,
    ) -> bool {
        let (Some(start), Some(end)) = (start_const, end_const) else {
            return false;
        };
        if !inclusive {
            return false;
        }
        start == end
    }
}
