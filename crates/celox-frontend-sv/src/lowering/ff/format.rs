//! Formatting data to a string (IEEE 1800-2023 21.3.3).
//!
//! `$sformatf` is a host request: the host renders the format and its
//! arguments with the formatter of `$display` and returns the result as a
//! string. A literal format is known here; any other format is a value that
//! the host reads as text when the call runs.

use super::*;
use celox_design::{HostRequestKind, HostRequestSite, HostRequestValue, ValueKind};

impl Ff<'_, '_> {
    /// The string `$sformatf(args[0], args[1..])` returns, as a temporary.
    pub(super) fn format_string(
        &mut self,
        ticks_per_unit: u64,
        args: &[sv::ir::Expr],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let (format, values) = args
            .split_first()
            .ok_or_else(|| unsupported("`$sformatf` without a format"))?;
        let template = literal_text(format);
        // Text without conversions is its own result.
        if values.is_empty() && template.as_deref().is_some_and(|text| !text.contains('%')) {
            return self.hoist(&sv::ir::Expr::Call {
                callee: sv::ir::Callee::String(sv::ir::StringOp::FromBits),
                args: vec![Some(format.clone())],
                arg_order: Vec::new(),
            });
        }
        if self.kernel.is_none() {
            return Err(unsupported("`$sformatf` outside a scheduled process"));
        }
        let mut values = values
            .iter()
            .cloned()
            .map(sv::ir::SystemTaskArg::Expr)
            .collect::<Vec<_>>();
        if let Some(template) = &template {
            // As the analyzer does for `$display`, scale each value that a
            // `%t` converts to simulation time units.
            scale_time_values(template, &mut values, ticks_per_unit);
            prepare_time_operands(self.m, Some(template), &mut values)?;
        }
        // Each value is captured where it is evaluated, so that a later
        // argument's call or host request cannot change it.
        let mut inputs = Vec::with_capacity(values.len() + 1);
        if template.is_none() {
            inputs.push(self.format_input(format)?);
        }
        for value in &values {
            let sv::ir::SystemTaskArg::Expr(value) = value else {
                unreachable!("format values are expressions");
            };
            inputs.push(self.format_input(value)?);
        }
        let (result, name) = self
            .m
            .temp("format", celox_design::HANDLE_WIDTH, false, false);
        self.m
            .variables
            .get_mut(&result)
            .expect("format result")
            .value_kind = ValueKind::StrRef;
        let site = self
            .host_request_sites
            .iter()
            .filter(|site| site.kind == HostRequestKind::Format)
            .count() as u32;
        // A literal format is the site's text; a run-time format is its first
        // argument, and the site keeps the time scale of its `%t` values.
        let (operation, text) = match template {
            Some(template) => (0, template),
            None => (1, ticks_per_unit.to_string()),
        };
        self.host_request_sites.push(HostRequestSite {
            process: 0,
            kind: HostRequestKind::Format,
            operation,
            args: inputs,
            results: vec![HostRequestValue {
                value_kind: ValueKind::StrRef,
                float_precision: None,
                id: result,
                width: celox_design::HANDLE_WIDTH,
                signed: false,
                is_string: false,
            }],
            strings: vec![text],
        });
        self.with_kernel(|kernel| kernel.host_request(HostRequestKind::Format, site))
            .map_err(kernel_error)?;
        Ok(sv::ir::Expr::Ident(name))
    }

    /// `expr` captured in a temporary that the host reads. A string keeps
    /// its handle, which also keeps the string alive until the host reads it.
    fn format_input(
        &mut self,
        expr: &sv::ir::Expr,
    ) -> Result<HostRequestValue<SourceVarId>, sv::AnalyzerError> {
        let expr = if self.m.calls(expr) {
            self.hoist(expr)?
        } else {
            expr.clone()
        };
        let value_kind = if self.m.expr_is_string(&expr) {
            ValueKind::StrRef
        } else {
            ValueKind::Bits
        };
        let float_precision = floating::precision(&expr, self.m.variables, self.m.name_to_id);
        let signed = self.expr_signed(&expr);
        let value = self.eval(&expr, None)?;
        let width = self.b.register(&value).width();
        let four_state = matches!(self.b.register(&value), RegisterType::Logic { .. });
        let (id, _) = self.m.temp("format_arg", width, signed, four_state);
        self.m
            .variables
            .get_mut(&id)
            .expect("format argument")
            .value_kind = value_kind;
        self.store(id, SIROffset::Static(0), width, value);
        Ok(HostRequestValue {
            value_kind,
            float_precision,
            id,
            width,
            signed,
            is_string: false,
        })
    }
}

/// The text of a literal format: its bytes, most significant first, without
/// NUL bytes, as a string literal holds them (IEEE 1800-2023 5.9).
fn literal_text(format: &sv::ir::Expr) -> Option<String> {
    let sv::ir::Expr::Literal(text) = format else {
        return None;
    };
    let literal = sv::typecheck::parse_integral_literal(text)?;
    if !literal.mask.is_zero() {
        return None;
    }
    let bytes = literal
        .value
        .to_bytes_be()
        .into_iter()
        .filter(|&byte| byte != 0)
        .collect::<Vec<_>>();
    String::from_utf8(bytes).ok()
}

/// Scale each value that a `%t` of `template` converts by `ticks_per_unit`,
/// as the analyzer scales the values of a `$display` (IEEE 1800-2023 20.4.3).
fn scale_time_values(
    template: &str,
    values: &mut [sv::ir::SystemTaskArg<sv::ir::Expr>],
    ticks_per_unit: u64,
) {
    if ticks_per_unit == 1 {
        return;
    }
    for (index, spec) in format_conversions(template).into_iter().enumerate() {
        if !matches!(spec, 't' | 'T') {
            continue;
        }
        if let Some(sv::ir::SystemTaskArg::Expr(value)) = values.get_mut(index) {
            *value = sv::ir::Expr::Binary {
                left: Box::new(value.clone()),
                op: sv::ir::BinaryOp::Mul,
                right: Box::new(sv::ir::Expr::Literal(format!("64'sd{ticks_per_unit}"))),
            };
        }
    }
}

/// The conversions of `template` that take an argument, in order
/// (IEEE 1800-2023 21.2.1.2): flags, a width and a precision may precede
/// each.
fn format_conversions(template: &str) -> Vec<char> {
    let mut conversions = Vec::new();
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
        if let Some(spec) = chars.next()
            && "bBoOhHxXdDiIcCsStTeEfFgG".contains(spec)
        {
            conversions.push(spec);
        }
    }
    conversions
}
