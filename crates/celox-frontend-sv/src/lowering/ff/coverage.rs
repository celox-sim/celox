//! Coverage operations of covergroups (IEEE 1800-2023 clause 19), lowered
//! to calls of the coverage helper.
//!
//! The helper is the heap helper under a `func` of its own
//! (`extern_abi::COVERAGE_HELPER_ID`): the heap keeps what covergroups
//! collect, and a coverage operation reads and writes nothing of the
//! simulation state, which the scheduler tells by the `func`. So a unit
//! that samples in clocked logic is planned as one without the call.

use super::*;
use celox_heap::coverage::{self, op};
use sv::ir::{CoverageOp, CoverageQuery, HandleProperty};

impl Ff<'_, '_> {
    /// Call the coverage helper with coverage operation `sub`, its
    /// immediate operands `x` and `y`, and up to three operands; the result
    /// is a 64-bit register.
    fn coverage_helper_call(
        &mut self,
        sub: u64,
        x: u64,
        y: u64,
        operands: &[RegisterId],
    ) -> RegisterId {
        let code = coverage::code(sub, x, y);
        debug_assert_eq!(code & 0xff, coverage::COVERAGE);
        let mut args = vec![self.constant64(code)];
        args.extend_from_slice(operands);
        while args.len() < celox_sir::extern_abi::HEAP_HELPER_ARGUMENTS {
            args.push(self.constant64(0));
        }
        let dst = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::ExternCall {
            dst: Some(dst),
            func: celox_sir::extern_abi::COVERAGE_HELPER_ID,
            args,
        });
        dst
    }

    /// The 64-bit words of the planes of `expr` as a value of `width` bits,
    /// from the least significant: the value and, for a value that may hold
    /// x or z bits, the mask, which is 0 for any other. The expression is
    /// evaluated once.
    fn coverage_words(
        &mut self,
        expr: &sv::ir::Expr,
        width: usize,
        signed: bool,
    ) -> Result<Vec<(RegisterId, RegisterId)>, sv::AnalyzerError> {
        let expr = self.hoist(expr)?;
        let value = self.eval(&expr, Some((width, signed)))?;
        let four_state = self.m.four_state
            && matches!(
                self.b.register(&value),
                celox_sir::RegisterType::Logic { .. }
            );
        let (value, mask) = if four_state {
            let (value, mask) = self.split_wide_planes(value, width);
            (value, Some(mask))
        } else {
            let known = self.b.alloc_bit(width, false);
            self.b
                .emit(SIRInstruction::Unary(known, UnaryOp::ToTwoState, value));
            (known, None)
        };
        let mut words = Vec::new();
        for first in (0..width).step_by(64) {
            let bits = (width - first).min(64);
            let word = |this: &mut Self, plane: RegisterId| {
                let word = this.b.alloc_bit(bits, false);
                this.b.emit(SIRInstruction::Slice(word, plane, first, bits));
                let wide = this.b.alloc_bit(64, false);
                this.b
                    .emit(SIRInstruction::Unary(wide, UnaryOp::Ident, word));
                wide
            };
            let value = word(self, value);
            let mask = match mask {
                Some(mask) => word(self, mask),
                None => self.constant64(0),
            };
            words.push((value, mask));
        }
        Ok(words)
    }

    /// A coverage operation that hands the heap a value of the design or
    /// reads one back (see [`CoverageOp`]): the values of a construction
    /// and of a sample, the arguments an instance keeps, and the text of
    /// its warnings. An operation that only hands a value over has none.
    fn coverage_value_call(
        &mut self,
        op: &CoverageOp,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let arg = |index: usize| {
            args.get(index)
                .and_then(Option::as_ref)
                .ok_or_else(|| unsupported("omitted argument of a coverage operation"))
        };
        // Every operand is evaluated before the first call, so that the
        // calls of a sample follow each other.
        let done = sv::ir::Expr::Literal("32'd0".into());
        match op {
            CoverageOp::Input {
                index: slot,
                width,
                signed,
            }
            | CoverageOp::FormalSet {
                slot,
                width,
                signed,
            }
            | CoverageOp::Point {
                item: slot,
                width,
                signed,
            } => {
                let sub = match op {
                    CoverageOp::Input { .. } => op::INPUT,
                    CoverageOp::FormalSet { .. } => op::FORMAL_SET,
                    _ => op::POINT,
                };
                let words = self.coverage_words(arg(1)?, *width as usize, *signed)?;
                let object = self.handle(arg(0)?)?;
                for (word, (value, mask)) in words.into_iter().enumerate() {
                    self.coverage_helper_call(
                        sub,
                        u64::from(*slot),
                        word as u64,
                        &[object, value, mask],
                    );
                }
                Ok(done)
            }
            CoverageOp::InputString { index } => {
                let string = self.handle(arg(1)?)?;
                let object = self.handle(arg(0)?)?;
                self.coverage_helper_call(
                    op::INPUT_STRING,
                    u64::from(*index),
                    0,
                    &[object, string],
                );
                Ok(done)
            }
            CoverageOp::Points {
                first,
                count,
                width,
            } => {
                let words = self.coverage_words(arg(1)?, *width as usize, false)?;
                let &[(values, masks)] = words.as_slice() else {
                    return Err(unsupported(
                        "coverage operation with coverpoint values of more than 64 bits",
                    ));
                };
                let present = match args.get(2).and_then(Option::as_ref) {
                    Some(present) => {
                        Some(self.integer_of_width(present, *count as usize, false)?)
                    }
                    None => None,
                };
                let object = self.handle(arg(0)?)?;
                if let Some(present) = present {
                    self.coverage_helper_call(
                        op::PRESENT,
                        u64::from(*first),
                        u64::from(*count),
                        &[object, present],
                    );
                }
                self.coverage_helper_call(
                    op::POINTS,
                    u64::from(*first),
                    u64::from(*count),
                    &[object, values, masks],
                );
                Ok(done)
            }
            CoverageOp::Formal {
                slot,
                width,
                signed,
                four_state,
            } => {
                let width = *width as usize;
                let four_state = *four_state && self.m.four_state;
                let object = self.handle(arg(0)?)?;
                // The planes of the value, each in words from the least
                // significant.
                let planes = if four_state { 2 } else { 1 };
                let mut read = Vec::new();
                for plane in 0..planes {
                    let mut words = Vec::new();
                    for (word, first) in (0..width).step_by(64).enumerate() {
                        let bits = (width - first).min(64);
                        let loaded = self.coverage_helper_call(
                            op::FORMAL,
                            u64::from(*slot),
                            2 * word as u64 + plane,
                            &[object],
                        );
                        let part = self.b.alloc_bit(bits, false);
                        self.b.emit(SIRInstruction::Slice(part, loaded, 0, bits));
                        words.push(part);
                    }
                    // A concatenation lists the most significant word
                    // first.
                    words.reverse();
                    let value = self.b.alloc_bit(width, false);
                    self.b.emit(SIRInstruction::Concat(value, words));
                    read.push(value);
                }
                let value = if four_state {
                    self.join_wide_planes(read[0], read[1], width)
                } else {
                    let value = self.b.alloc_bit(width, *signed);
                    self.b
                        .emit(SIRInstruction::Unary(value, UnaryOp::Ident, read[0]));
                    value
                };
                let (temp, name) = self.m.temp("coverage", width, *signed, four_state);
                self.store(temp, SIROffset::Static(0), width, value);
                Ok(sv::ir::Expr::Ident(name))
            }
            CoverageOp::Message => {
                let object = self.handle(arg(0)?)?;
                let result = self.coverage_helper_call(op::MESSAGE, 0, 0, &[object]);
                let width = celox_design::HANDLE_WIDTH;
                let value = self.b.alloc_bit(width, false);
                self.b.emit(SIRInstruction::Slice(value, result, 0, width));
                let (temp, name) = self.m.temp("coverage", width, false, false);
                self.m.variables.get_mut(&temp).unwrap().value_kind =
                    celox_design::ValueKind::StrRef;
                self.store(temp, SIROffset::Static(0), width, value);
                Ok(sv::ir::Expr::Ident(name))
            }
            _ => unreachable!("a coverage operation without a value operand"),
        }
    }

    /// The value of coverage operation `op`, as a temporary.
    pub(super) fn coverage_call(
        &mut self,
        op: &CoverageOp,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        // `$get_coverage` of a design that constructs no covergroup is 100
        // (19.11), with no call.
        if matches!(op, CoverageOp::Overall) && self.m.no_covergroups {
            return Ok(sv::ir::Expr::Literal("100.0".into()));
        }
        if matches!(
            op,
            CoverageOp::Input { .. }
                | CoverageOp::InputString { .. }
                | CoverageOp::FormalSet { .. }
                | CoverageOp::Formal { .. }
                | CoverageOp::Points { .. }
                | CoverageOp::Point { .. }
                | CoverageOp::Message
        ) {
            return self.coverage_value_call(op, args);
        }
        // The first operand is the key of a type or the handle of an
        // instance; the second names an item.
        let keyed = matches!(
            op,
            CoverageOp::Registered
                | CoverageOp::Register(_)
                | CoverageOp::New { .. }
                | CoverageOp::TypeCoverage { .. }
        );
        let mut args = args
            .iter()
            .map(|arg| {
                arg.clone()
                    .ok_or_else(|| unsupported("omitted argument of a coverage operation"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if args.iter().any(|arg| self.m.calls(arg)) {
            for (index, arg) in args.iter_mut().enumerate() {
                *arg = if index == 0 && keyed {
                    self.heap_operand(arg, Some((64, false)), None)?
                } else if index == 0 {
                    self.heap_operand(
                        arg,
                        Some((celox_design::HANDLE_WIDTH, false)),
                        Some(celox_design::ValueKind::ObjRef),
                    )?
                } else {
                    self.heap_operand(arg, Some((32, false)), None)?
                };
            }
        }
        let first = match args.first() {
            Some(first) if keyed => Some(self.integer_of_width(first, 64, false)?),
            Some(first) => Some(self.handle(first)?),
            None => None,
        };
        let first = |operation: &str| {
            first.ok_or_else(|| {
                unsupported(format!("coverage operation {operation} without an operand"))
            })
        };
        let result = match op {
            CoverageOp::Registered => {
                let key = first("registered")?;
                self.coverage_helper_call(op::REGISTERED, 0, 0, &[key])
            }
            CoverageOp::Register(descriptor) => {
                let key = first("register")?;
                // The descriptor travels as its JSON text, eight bytes a
                // call, to the heap that then registers it.
                let bytes = serde_json::to_vec(&**descriptor)
                    .map_err(|error| unsupported(format!("covergroup descriptor: {error}")))?;
                for chunk in bytes.chunks(8) {
                    let word = chunk
                        .iter()
                        .fold(0u64, |word, byte| (word << 8) | u64::from(*byte));
                    let word = self.constant64(word);
                    self.coverage_helper_call(
                        op::REGISTER_BYTES,
                        chunk.len() as u64,
                        0,
                        &[key, word],
                    );
                }
                self.coverage_helper_call(op::REGISTER, 0, 0, &[key])
            }
            CoverageOp::New {
                class,
                size,
                handles,
            } => {
                let key = first("new")?;
                let size = self.constant64(u64::from(*size));
                let class = self.constant64(u64::from(*class));
                let object = self.coverage_helper_call(op::NEW, 0, 0, &[key, size, class]);
                for &(offset, kind) in handles {
                    let offset = self.constant64(u64::from(offset));
                    let kind = self.constant64(match kind {
                        HandleProperty::String => 0,
                        HandleProperty::Object => 1,
                    });
                    self.heap_call(celox_heap::ops::OBJECT_HANDLE, &[object, offset, kind]);
                }
                object
            }
            CoverageOp::Build => {
                let object = first("build")?;
                self.coverage_helper_call(op::BUILD, 0, 0, &[object])
            }
            CoverageOp::Sampling => {
                let object = first("sampling")?;
                self.coverage_helper_call(op::SAMPLING, 0, 0, &[object])
            }
            CoverageOp::SampleEnd => {
                let object = first("sample")?;
                self.coverage_helper_call(op::SAMPLE_END, 0, 0, &[object])
            }
            CoverageOp::InstCoverage { what } | CoverageOp::TypeCoverage { what } => {
                let target = first("query")?;
                let item = match args.get(1) {
                    Some(item) => self.integer_of_width(item, 32, false)?,
                    None => self.constant64(0),
                };
                let what = match what {
                    CoverageQuery::Real => 0,
                    CoverageQuery::Covered => 1,
                    CoverageQuery::Total => 2,
                };
                let sub = if matches!(op, CoverageOp::InstCoverage { .. }) {
                    op::INST_COVERAGE
                } else {
                    op::TYPE_COVERAGE
                };
                self.coverage_helper_call(sub, what, 0, &[target, item])
            }
            CoverageOp::Overall => self.coverage_helper_call(op::OVERALL, 0, 0, &[]),
            CoverageOp::Input { .. }
            | CoverageOp::InputString { .. }
            | CoverageOp::FormalSet { .. }
            | CoverageOp::Formal { .. }
            | CoverageOp::Points { .. }
            | CoverageOp::Point { .. }
            | CoverageOp::Message => unreachable!("handled by coverage_value_call"),
        };
        let (width, signed) = op.result_type();
        let value = self.b.alloc_bit(width, signed);
        self.b.emit(SIRInstruction::Slice(value, result, 0, width));
        let (temp, name) = self.m.temp("coverage", width, signed, false);
        let variable = self.m.variables.get_mut(&temp).unwrap();
        if matches!(op, CoverageOp::New { .. }) {
            variable.value_kind = celox_design::ValueKind::ObjRef;
        }
        // A coverage is a `real`, whose bits the helper returns.
        if op.is_real() {
            variable.float = Some(celox_sir::FloatPrecision::F64);
        }
        self.store(temp, SIROffset::Static(0), width, value);
        Ok(sv::ir::Expr::Ident(name))
    }
}
