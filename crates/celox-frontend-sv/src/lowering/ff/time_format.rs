//! The format of `%t` (IEEE 1800-2023 20.4.3).
//!
//! `$timeformat` stores its format in a variable of the design's state,
//! [`TIME_FORMAT`], which every module of a design that calls it shares.
//! Each site that converts a time with `%t` captures the variable with its
//! arguments, so an event is formatted as the format was when the site ran,
//! and checkpoints, restores and mirrors carry the format with the rest of
//! the state.

use super::*;
use celox_design::time_format::{
    FIELD_WIDTH, MAX_FIELD_WIDTH, MAX_PRECISION, PRECISION, SET, SUFFIX, SUFFIX_BYTES,
    SUFFIX_LENGTH, UNITS,
};

/// The units numbers `$timeformat` accepts: 100 s to 1 fs (IEEE 1800-2023
/// 20.4.3).
const MIN_UNITS: i32 = -15;
const MAX_UNITS: i32 = 2;

/// The package of the [`TIME_FORMAT`] variable, and the variable itself.
pub(crate) const TIME_FORMAT: &str = "$timeformat";

/// Whether `module` calls `$timeformat`, which makes the design share the
/// [`TIME_FORMAT`] variable.
pub(crate) fn calls_time_format(module: &sv::ir::Module) -> bool {
    let mut found = false;
    let mut visit = |stmts: &[sv::ir::Stmt]| {
        for stmt in stmts {
            stmt.walk(&mut |stmt| {
                if let sv::ir::Stmt::SystemTask {
                    task: sv::ir::SystemTask::Timeformat,
                    ..
                } = stmt
                {
                    found = true;
                }
            });
        }
    };
    for process in module.initial_processes() {
        visit(process.body());
    }
    for process in module.ff_processes() {
        visit(process.body());
    }
    for process in module.comb_processes() {
        visit(process.body());
    }
    for subroutine in module.subroutines() {
        visit(&subroutine.body);
    }
    found
}

/// Whether `template` converts a time with `%t`.
pub(in crate::lowering) fn converts_time(template: &str) -> bool {
    let mut chars = template.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            continue;
        }
        while chars
            .next_if(|ch| matches!(ch, '-' | '+' | ' ' | '#' | '0'))
            .is_some()
        {}
        while chars.next_if(char::is_ascii_digit).is_some() {}
        if chars.next_if_eq(&'.').is_some() {
            while chars.next_if(char::is_ascii_digit).is_some() {}
        }
        if matches!(chars.next(), Some('t' | 'T')) {
            return true;
        }
    }
    false
}

/// The [`TIME_FORMAT`] variable as an operand of a site whose format
/// `converts` a time, when the design calls `$timeformat`.
pub(in crate::lowering) fn time_format_operand(
    module: &ProcModule<'_>,
    converts: bool,
) -> Option<sv::ir::Expr> {
    (converts && module.id(TIME_FORMAT).is_some())
        .then(|| sv::ir::Expr::Ident(TIME_FORMAT.to_string()))
}

impl Ff<'_, '_> {
    /// `$timeformat(units, precision, suffix, width)`, to which the
    /// analyzer appended the power of ten seconds of the simulation's time
    /// unit: it stores the format, with the units counted from the time
    /// unit, in the [`TIME_FORMAT`] variable. Units outside 2 to -15 end the
    /// simulation with an error, as a constant outside them fails the
    /// analysis (IEEE 1800-2023 20.4.3), and so do a negative precision or
    /// field width; Celox prints at most [`MAX_PRECISION`] decimal places
    /// and pads to at most [`MAX_FIELD_WIDTH`] characters, and a larger one
    /// is an error that says so.
    pub(super) fn time_format(
        &mut self,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
    ) -> Result<(), sv::AnalyzerError> {
        use sv::ir::SystemTaskArg;
        let id = self
            .m
            .id(TIME_FORMAT)
            .ok_or_else(|| unsupported("`$timeformat` without its format variable"))?;
        let [
            SystemTaskArg::Expr(units),
            SystemTaskArg::Expr(precision),
            suffix,
            SystemTaskArg::Expr(width),
            SystemTaskArg::Expr(tick),
        ] = args
        else {
            return Err(unsupported("`$timeformat` arguments"));
        };
        let suffix = match suffix {
            SystemTaskArg::Str(text) => unescape(text)?,
            // A packed value's leading zero bytes are its padding, which
            // it does not tell from NUL characters (IEEE 1800-2023 11.10.2);
            // a NUL after its first character stays.
            SystemTaskArg::Expr(sv::ir::Expr::Literal(text)) => {
                sv::typecheck::parse_integral_literal(text)
                    .map(|literal| {
                        let bytes = literal.value.to_bytes_be();
                        if bytes == [0] { Vec::new() } else { bytes }
                    })
                    .ok_or_else(|| unsupported("`$timeformat` suffix that is not a literal"))?
            }
            SystemTaskArg::Expr(_) => {
                return Err(unsupported("`$timeformat` suffix that is not a literal"));
            }
            SystemTaskArg::Empty => Vec::new(),
        };
        if suffix.len() > SUFFIX_BYTES {
            return Err(unsupported(format!(
                "`$timeformat` suffix longer than {SUFFIX_BYTES} characters"
            )));
        }
        let suffix_length = suffix.len();
        // Every argument is evaluated before the format changes, so that
        // a call in one sees the format before the call (IEEE 1800-2023
        // 20.4.3). As for a display, the calls run first, and an argument
        // before the last one with a call is kept in a temporary, which a
        // host request of a later call does not lose.
        let mut fields = vec![
            (UNITS, units.clone()),
            (PRECISION, precision.clone()),
            (FIELD_WIDTH, width.clone()),
        ];
        let last_call = fields.iter().rposition(|(_, expr)| self.m.calls(expr));
        if let Some(last_call) = last_call {
            for (index, (_, expr)) in fields[..=last_call].iter_mut().enumerate() {
                let hoisted = if self.m.calls(expr) {
                    self.hoist(expr)?
                } else {
                    expr.clone()
                };
                *expr = if index == last_call || matches!(hoisted, sv::ir::Expr::Literal(_)) {
                    hoisted
                } else {
                    self.keep_argument(&hoisted)?
                };
            }
        }
        let mut values = fields
            .into_iter()
            .map(|(offset, value)| Ok((offset, self.eval(&value, Some((32, true)))?)))
            .collect::<Result<Vec<_>, sv::AnalyzerError>>()?;
        let [(_, units), (_, precision), (_, width)] = values[..] else {
            unreachable!("three numeric fields");
        };
        self.fail_outside(
            units,
            MIN_UNITS,
            MAX_UNITS,
            format!(
                "`$timeformat` units number is outside {MIN_UNITS}..{MAX_UNITS} (IEEE 1800-2023 20.4.3)"
            ),
        );
        for (value, name, most) in [
            (precision, "precision number", MAX_PRECISION),
            (width, "minimum field width", MAX_FIELD_WIDTH),
        ] {
            self.fail_outside(
                value,
                0,
                i32::MAX,
                format!("`$timeformat` {name} is outside 0.. (IEEE 1800-2023 20.4.3)"),
            );
            self.fail_outside(
                value,
                0,
                most,
                format!("`$timeformat` {name} above {most} is not supported"),
            );
        }
        let tick = self.eval(tick, Some((32, true)))?;
        let counted = self.b.alloc_bit(32, true);
        self.b
            .emit(SIRInstruction::Binary(counted, units, BinaryOp::Sub, tick));
        values[0].1 = counted;
        let bits = suffix.iter().fold(BigUint::zero(), |bits, byte| {
            (bits << 8u32) | BigUint::from(*byte)
        });
        let suffix = self.b.alloc_bit(8 * SUFFIX_BYTES, false);
        self.b
            .emit(SIRInstruction::Imm(suffix, SIRValue::new(bits)));
        for (offset, value) in values {
            self.store(id, SIROffset::Static(offset), 32, value);
        }
        self.store(id, SIROffset::Static(SUFFIX), 8 * SUFFIX_BYTES, suffix);
        let length = self.b.alloc_bit(8, false);
        self.b.emit(SIRInstruction::Imm(
            length,
            SIRValue::new(suffix_length as u8),
        ));
        self.store(id, SIROffset::Static(SUFFIX_LENGTH), 8, length);
        let set = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Imm(set, SIRValue::new(1u8)));
        self.store(id, SIROffset::Static(SET), 1, set);
        Ok(())
    }

    /// End the simulation with the error `message` unless the 32-bit signed
    /// `value` is from `low` to `high`.
    fn fail_outside(&mut self, value: RegisterId, low: i32, high: i32, message: String) {
        let mut compare = |op, bound: i32| {
            let immediate = self.b.alloc_bit(32, true);
            self.b.emit(SIRInstruction::Imm(
                immediate,
                SIRValue::new(BigUint::from(bound as u32)),
            ));
            let result = self.b.alloc_bit(1, false);
            self.b
                .emit(SIRInstruction::Binary(result, value, op, immediate));
            result
        };
        let (below, above) = (compare(BinaryOp::LtS, low), compare(BinaryOp::GtS, high));
        let outside = self.b.alloc_bit(1, false);
        self.b
            .emit(SIRInstruction::Binary(outside, below, BinaryOp::Or, above));
        let error = self.b.new_block();
        let inside = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: outside,
            true_block: (error, Vec::new()),
            false_block: (inside, Vec::new()),
        });
        self.b.switch_to_block(error);
        let code = self.m.runtime_error(message);
        self.b.seal_block(SIRTerminator::Error(code));
        self.b.switch_to_block(inside);
    }
}
