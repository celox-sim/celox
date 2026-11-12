//! Formats of the display, write and string formatting tasks (IEEE 1800-2023 21.2.1, 21.3.3).
use super::*;

impl BodyBuilder<'_, '_, '_> {
    /// Fold the arguments of a display or write task after its first format
    /// into that format when a later string literal is a further format: a
    /// string literal that no conversion consumes continues the format, and
    /// a value without a conversion of its own is written in the radix of the
    /// task (IEEE 1800-2023 21.2.1.1).
    pub(super) fn merge_display_formats(
        &self,
        task: SystemTask,
        args: &mut Vec<SystemTaskArg<Expr>>,
        ids: &mut Vec<Option<celox_sv_syntax::ExprId>>,
    ) {
        let (SystemTask::Display(radix)
        | SystemTask::Strobe(radix)
        | SystemTask::Monitor(radix)
        | SystemTask::Fmonitor(radix)
        | SystemTask::Fstrobe(radix)
        | SystemTask::Write(radix)
        | SystemTask::Fdisplay(radix)
        | SystemTask::Fwrite(radix)) = task
        else {
            return;
        };
        let Some(start) = template_index(task, args) else {
            return;
        };
        let Some(SystemTaskArg::Str(first)) = args.get(start) else {
            return;
        };
        let mut text = first.clone();
        let mut pending = format_conversions(&text).len();
        let mut later_format = false;
        for arg in &args[start + 1..] {
            match arg {
                SystemTaskArg::Str(literal) if pending == 0 => {
                    later_format = true;
                    pending = format_conversions(literal).len();
                    text.push_str(literal);
                }
                _ if pending > 0 => pending -= 1,
                SystemTaskArg::Expr(value) => text.push_str(&default_conversion(
                    expression_binding::string_value(value, &self.dims),
                    expression_binding::real_value(value, &self.dims),
                    radix,
                )),
                SystemTaskArg::Str(_) => unreachable!("an unconsumed literal is a format"),
                SystemTaskArg::Empty => text.push_str(&default_conversion(false, false, radix)),
            }
        }
        if !later_format {
            return;
        }
        // The values keep their places; the further formats are folded into
        // the first one.
        let mut pending = format_conversions(first).len();
        let mut kept = vec![SystemTaskArg::Str(text)];
        let mut kept_ids = vec![ids.get(start).copied().flatten()];
        for (position, arg) in args.iter().enumerate().skip(start + 1) {
            match arg {
                SystemTaskArg::Str(literal) if pending == 0 => {
                    pending = format_conversions(literal).len();
                }
                arg => {
                    pending = pending.saturating_sub(1);
                    kept.push(arg.clone());
                    kept_ids.push(ids.get(position).copied().flatten());
                }
            }
        }
        args.truncate(start);
        args.extend(kept);
        ids.truncate(start);
        ids.extend(kept_ids);
    }

    /// Name the subroutine in each `%m` of the format of a display or write
    /// task (IEEE 1800-2023 21.2.1.5). The formats that follow the first are
    /// already merged into it.
    pub(super) fn scope_display_format(&self, task: SystemTask, args: &mut [SystemTaskArg<Expr>]) {
        if let Some(template) = template_index(task, args)
            && let Some(SystemTaskArg::Str(text)) = args.get_mut(template)
        {
            let scoped = expression_binding::scoped_template(
                text.as_bytes(),
                self.dims.scope_name.as_deref(),
            );
            *text = String::from_utf8(scoped).expect("names and formats are UTF-8");
        }
    }

    /// Rewrite each `%p` of the format of a display or write task into a
    /// conversion of the value it prints (IEEE 1800-2023 21.2.1.6).
    pub(super) fn rewrite_display_patterns(
        &mut self,
        task: SystemTask,
        args: &mut [SystemTaskArg<Expr>],
        ids: &[Option<celox_sv_syntax::ExprId>],
    ) -> Result<(), AnalyzerError> {
        let Some(template) = template_index(task, args) else {
            return Ok(());
        };
        let Some(SystemTaskArg::Str(text)) = args.get(template) else {
            return Ok(());
        };
        if !expression_binding::has_pattern(text.as_bytes()) {
            return Ok(());
        }
        let text = text.clone();
        let native = self.tree.declarations();
        let rewritten = expression_binding::rewrite_template(text.as_bytes(), |index| {
            let position = template + 1 + index;
            Ok(match args.get_mut(position) {
                Some(SystemTaskArg::Expr(value)) => {
                    let id = ids
                        .get(position)
                        .copied()
                        .flatten()
                        .ok_or_else(|| unsupported("omitted %p argument"))?;
                    if let Some(text) = expression_binding::format_aggregate_text(
                        self.tree,
                        self.session,
                        id,
                        value,
                        native,
                        &self.dims,
                    )? {
                        *value = text;
                        return Ok(&b"%s"[..]);
                    }
                    let pattern = expression_binding::format_pattern(
                        self.tree,
                        self.session,
                        id,
                        value,
                        native,
                        &self.dims,
                    )?;
                    let (conversion, argument) = expression_binding::format_pattern_argument(
                        pattern,
                        std::mem::replace(value, Expr::Literal("0".into())),
                        &self.dims,
                    );
                    *value = argument;
                    conversion
                }
                // A string literal prints in quotes.
                Some(SystemTaskArg::Str(literal)) => {
                    *literal = format!("\\\"{literal}\\\"");
                    b"%s"
                }
                Some(SystemTaskArg::Empty) | None => b"%0d",
            })
        })?;
        args[template] = SystemTaskArg::Str(
            String::from_utf8(rewritten).map_err(|_| unsupported("non-UTF-8 format"))?,
        );
        Ok(())
    }

    /// `output = value` for the string `value` of `$sformat`, which assigns
    /// its output as a string literal is assigned (IEEE 1800-2023 21.3.3,
    /// 5.9).
    pub(super) fn format_assignment(
        &mut self,
        output: celox_sv_syntax::ExprId,
        value: Expr,
    ) -> Result<Vec<Stmt>, AnalyzerError> {
        let native = self.tree.declarations();
        let property = expression_binding::object_property(
            self.tree,
            self.session,
            output,
            native,
            &self.dims,
        )?;
        let string = match &property {
            Some((property, _)) => {
                property.field.handle == Some(system_functions::HandleProperty::String)
            }
            None => expression_binding::native_string(
                self.tree,
                self.session,
                output,
                native,
                &self.dims,
            ),
        };
        // An integral output takes the string as a string literal assigned
        // to it (IEEE 1800-2023 21.3.3, 5.9): its characters right-justified,
        // the leftmost ones dropped when they do not fit, zeros on the left.
        let (mut stmts, value) = if string {
            (Vec::new(), value)
        } else {
            self.string_bits(output, value)?
        };
        // A string property is stored as a handle; any other output is
        // written as an assignment writes its target, a place through its
        // object, after the value (4.9.3).
        if string && let Some((property, receiver)) = property {
            let handle = expression_binding::object_receiver(
                self.tree,
                self.session,
                receiver,
                native,
                &self.dims,
            )?;
            stmts.push(self.property_store(&property, handle, "=", value)?);
            return Ok(stmts);
        }
        let saved = (
            self.dims.effect_values.clone(),
            self.dims.last_indices.clone(),
        );
        stmts.extend(self.target_write(output, value, false)?);
        (self.dims.effect_values, self.dims.last_indices) = saved;
        Ok(stmts)
    }

    /// The statements that put the bits of string `value` into a hidden
    /// local of the integral type of `output`, and that local: each
    /// character in turn shifts in from the right, so the last ones remain.
    pub(super) fn string_bits(
        &mut self,
        output: celox_sv_syntax::ExprId,
        value: Expr,
    ) -> Result<(Vec<Stmt>, Expr), AnalyzerError> {
        let current = expression_binding::value(
            output,
            self.tree.declarations(),
            self.tree,
            self.session,
            &self.dims,
        )?;
        if expression_binding::real_value(&current, &self.dims) {
            return Err(unsupported("`$sformat` output that is not a string"));
        }
        let ty = dimensions::bound_bits_type(self.tree, &current, &self.dims)
            .ok_or_else(|| unsupported("`$sformat` output that is not a string"))?;
        let mut bits = Type::new(TypeKind::Logic);
        if ty.width > 1 {
            bits.packed_ranges = vec![PackedRange::new(
                ConstExpr::Literal((ty.width - 1).to_string()),
                ConstExpr::Literal("0".into()),
            )];
        }
        let (text_source, text) =
            self.hidden(type_binding::builtin_type("string", None).expect("a builtin type"));
        let (bits_source, bits) = self.hidden(bits);
        let (_, index) =
            self.hidden(type_binding::builtin_type("int", None).expect("a builtin type"));
        let _ = text_source;
        let mut value = value;
        self.rename_expr(&mut value);
        let string = |op, args: Vec<Expr>| expression_binding::string_call(op, args);
        let character = Expr::Binary {
            left: Box::new(string(
                StringOp::Getc,
                vec![Expr::Ident(text.clone()), Expr::Ident(index.clone())],
            )),
            op: BinaryOp::BitAnd,
            right: Box::new(Expr::Literal("8'hff".into())),
        };
        let stmts = vec![
            Stmt::Assign {
                lhs: LValue::Ident(text.clone()),
                rhs: value,
                nonblocking: false,
            },
            Stmt::Assign {
                lhs: LValue::Ident(bits.clone()),
                rhs: Expr::Literal("0".into()),
                nonblocking: false,
            },
            Stmt::Loop {
                kind: LoopKind::For,
                init: vec![Stmt::Assign {
                    lhs: LValue::Ident(index.clone()),
                    rhs: Expr::Literal("0".into()),
                    nonblocking: false,
                }],
                condition: Some(Expr::Binary {
                    left: Box::new(Expr::Ident(index.clone())),
                    op: BinaryOp::Lt,
                    right: Box::new(string(StringOp::Len, vec![Expr::Ident(text.clone())])),
                }),
                step: vec![Stmt::Assign {
                    lhs: LValue::Ident(index.clone()),
                    rhs: Expr::Binary {
                        left: Box::new(Expr::Ident(index.clone())),
                        op: BinaryOp::Add,
                        right: Box::new(Expr::Literal("1".into())),
                    },
                    nonblocking: false,
                }],
                body: vec![Stmt::Assign {
                    lhs: LValue::Ident(bits.clone()),
                    rhs: Expr::Binary {
                        left: Box::new(Expr::Binary {
                            left: Box::new(Expr::Ident(bits.clone())),
                            op: BinaryOp::Shl,
                            right: Box::new(Expr::Literal("8".into())),
                        }),
                        op: BinaryOp::BitOr,
                        right: Box::new(character),
                    },
                    nonblocking: false,
                }],
            },
        ];
        Ok((stmts, Expr::Ident(bits_source)))
    }
}

/// The position of the format among the arguments of a display, write or
/// severity task, which a leading string literal there is.
pub(super) fn template_index(task: SystemTask, args: &[SystemTaskArg<Expr>]) -> Option<usize> {
    Some(match task {
        SystemTask::Display(_)
        | SystemTask::Strobe(_)
        | SystemTask::Monitor(_)
        | SystemTask::Write(_)
        | SystemTask::Severity(_) => 0,
        SystemTask::Fatal => usize::from(matches!(args.first(), Some(SystemTaskArg::Expr(_)))),
        SystemTask::Fdisplay(_)
        | SystemTask::Fstrobe(_)
        | SystemTask::Fmonitor(_)
        | SystemTask::Fwrite(_)
        | SystemTask::Assert
        | SystemTask::AssertContinue => 1,
        _ => return None,
    })
}

/// The conversion that writes a value without one of its own in a task of
/// `radix` (IEEE 1800-2023 21.2.1.1): a string as text, a real as `$write`
/// writes reals.
pub(super) fn default_conversion(
    string: bool,
    real: bool,
    radix: system_functions::Radix,
) -> String {
    if real {
        "%#g".into()
    } else if string {
        "%s".into()
    } else {
        format!("%{}", radix.letter())
    }
}

/// The name `%m` prints in the subroutine `name`, a method of `class` if
/// any, whose declaration spans `tokens` (IEEE 1800-2023 21.2.1.5). Names are
/// the source's, as `$typename` prints them: a package's subroutines are
/// named by the package, the compilation unit's by `$unit` (26.3), and a
/// design element's under the instance that runs them. It does not depend
/// on the caller.
pub(super) fn scope_name(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &SharedEnv,
    tokens: celox_sv_syntax::TokenRange,
    class: Option<&classes::Class>,
    name: &str,
) -> expression_binding::ScopeName {
    use expression_binding::ScopeName;
    let unit = classes::enclosing_unit(tree.declarations(), tokens);
    let path = match class
        .and_then(|class| type_names::class_scope_name(&class.name, tree, session, env))
    {
        Some(class) => format!("{class}::{name}"),
        None => type_names::scoped_name(unit, name),
    };
    // A design element's names are under the instance, which `%m` still
    // names at run time.
    match unit.filter(|unit| unit.kind != celox_sv_syntax::UnitKind::Package) {
        Some(unit) => {
            let element = format!("{}.", instances::normalize_identifier(&unit.name.spelling));
            ScopeName::Relative(path.strip_prefix(&element).unwrap_or(&path).to_owned())
        }
        None => ScopeName::Absolute(path),
    }
}

/// The radix of a `$swrite` task named `name`.
pub(super) fn swrite_radix(name: &str) -> Option<system_functions::Radix> {
    use system_functions::Radix;
    Some(match name {
        "$swrite" => Radix::Decimal,
        "$swriteb" => Radix::Binary,
        "$swriteo" => Radix::Octal,
        "$swriteh" => Radix::Hexadecimal,
        _ => return None,
    })
}

/// The conversions of a format, in the order they take arguments
/// (IEEE 1800-2023 21.2.1.2): flags, a width and a precision may precede
/// each.
pub(super) fn format_conversions(text: &str) -> Vec<char> {
    let mut conversions = Vec::new();
    let mut chars = text.chars().peekable();
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
            && "bBoOhHxXdDiIcCsStTeEfFgGpP".contains(spec)
        {
            conversions.push(spec);
        }
    }
    conversions
}

/// `%t` reports a time value in the units of `$timeformat`, by default the
/// smallest time precision of the design (IEEE 1800-2023 20.4.3, Table 20-3),
/// in which the simulation counts its time. The value is in the time unit of
/// the calling scope, of which `ticks_per_unit` simulation time units make
/// one, so each value a `%t` of the template converts is scaled by it.
pub(super) fn scale_time_conversions(
    task: SystemTask,
    args: &mut [SystemTaskArg<Expr>],
    ticks_per_unit: u64,
    bind: &mut impl FnMut(usize) -> Result<Option<Expr>, AnalyzerError>,
) -> Result<(), AnalyzerError> {
    if ticks_per_unit == 1 {
        return Ok(());
    }
    let Some(template) = template_index(task, args) else {
        return Ok(());
    };
    let Some(SystemTaskArg::Str(text)) = args.get(template) else {
        return Ok(());
    };
    for (offset, spec) in format_conversions(text).into_iter().enumerate() {
        if !matches!(spec, 't' | 'T') {
            continue;
        }
        let index = template + 1 + offset;
        // A string literal converts as its packed bytes (5.9). The factor is
        // signed, so the product keeps the signedness of the value (11.8.1).
        let value = match args.get(index) {
            Some(SystemTaskArg::Expr(value)) => value.clone(),
            Some(SystemTaskArg::Str(_)) => match bind(index)? {
                Some(value) => value,
                None => continue,
            },
            Some(SystemTaskArg::Empty) | None => continue,
        };
        args[index] = SystemTaskArg::Expr(Expr::Binary {
            left: Box::new(value),
            op: BinaryOp::Mul,
            right: Box::new(Expr::Literal(format!("64'sd{ticks_per_unit}"))),
        });
    }
    Ok(())
}

/// The arguments of `$timeformat` (IEEE 1800-2023 20.4.3): the units
/// number, the precision, the suffix and the minimum field width, without
/// arguments the defaults of Table 20-3, then the power of ten seconds
/// `tick` of the simulation's time unit, from which the frontend counts the
/// units.
pub(super) fn timeformat_arguments(
    args: Vec<SystemTaskArg<Expr>>,
    tick: i32,
) -> Result<Vec<SystemTaskArg<Expr>>, AnalyzerError> {
    let literal = |value: u32| Expr::Literal(format!("32'sd{value}"));
    // A literal that is never negative, subtracted from zero if need be.
    let signed = |value: i32| {
        if value < 0 {
            Expr::Binary {
                left: Box::new(literal(0)),
                op: BinaryOp::Sub,
                right: Box::new(literal(value.unsigned_abs())),
            }
        } else {
            literal(value.unsigned_abs())
        }
    };
    let mut args = match args.len() {
        0 => vec![
            SystemTaskArg::Expr(signed(tick)),
            SystemTaskArg::Expr(literal(0)),
            SystemTaskArg::Str(String::new()),
            SystemTaskArg::Expr(literal(20)),
        ],
        4 => args,
        count => {
            return Err(AnalyzerError::InvalidSystemTfCall {
                name: "$timeformat".into(),
                detail: format!(
                    "{count} arguments instead of none or four (IEEE 1800-2023 20.4.3)"
                ),
            });
        }
    };
    if !matches!(
        (&args[0], &args[1], &args[3]),
        (
            SystemTaskArg::Expr(_),
            SystemTaskArg::Expr(_),
            SystemTaskArg::Expr(_)
        )
    ) {
        return Err(AnalyzerError::InvalidSystemTfCall {
            name: "$timeformat".into(),
            detail: "the units, precision and minimum field width are integers".into(),
        });
    }
    args.push(SystemTaskArg::Expr(signed(tick)));
    Ok(args)
}
