//! Calls of the UVM DPI-C functions Celox provides (see
//! [`sv::ir::DpiBuiltin`]), lowered to heap operations instead of C calls.

use super::*;
use celox_heap::ops;
use sv::ir::{DpiBuiltin, DpiType, StringOp};

impl Ff<'_, '_> {
    /// Call builtin `builtin` of `import`, returning the register holding its
    /// result.
    pub(super) fn builtin_dpi_call(
        &mut self,
        import: &sv::ir::DpiImport,
        builtin: DpiBuiltin,
        args: &[Option<sv::ir::Expr>],
        arg_order: &[usize],
    ) -> Result<Option<RegisterId>, sv::AnalyzerError> {
        let name = import.name();
        let positions: Vec<_> = if arg_order.is_empty() {
            (0..args.len()).collect()
        } else {
            arg_order.to_vec()
        };
        // Inputs in source order, each kept in a temporary before any heap
        // call (see `heap_operand`).
        let mut inputs = vec![None; args.len()];
        for position in positions {
            let argument = &import.arguments()[position];
            if argument.is_output() {
                continue;
            }
            let arg = args[position].as_ref().ok_or_else(|| {
                unsupported(format!("omitted argument of DPI-C function `{name}`"))
            })?;
            let arg = if argument.r#type() == DpiType::String {
                self.string_argument(arg)?
            } else {
                let r#type = argument.r#type();
                self.heap_operand(arg, Some((r#type.width(), r#type.is_signed())), None)?
            };
            inputs[position] = Some(arg);
        }
        let operand = |this: &mut Self, position: usize| -> Result<RegisterId, sv::AnalyzerError> {
            let arg = inputs[position].as_ref().expect("an input argument");
            let r#type = import.arguments()[position].r#type();
            if r#type == DpiType::String {
                this.handle(arg)
            } else {
                this.integer_of_width(arg, r#type.width(), r#type.is_signed())
            }
        };
        let call =
            |this: &mut Self, code: u64, operands: &[RegisterId]| this.heap_call(code, operands);
        let result = match builtin {
            DpiBuiltin::ReComp | DpiBuiltin::ReExec | DpiBuiltin::ReDeglobbed => {
                let a = operand(self, 0)?;
                let b = operand(self, 1)?;
                let code = match builtin {
                    DpiBuiltin::ReComp => ops::UVM_RE_COMP,
                    DpiBuiltin::ReExec => ops::UVM_RE_EXEC,
                    _ => ops::UVM_RE_DEGLOBBED,
                };
                Some(call(self, code, &[a, b]))
            }
            DpiBuiltin::ReFree => {
                let a = operand(self, 0)?;
                call(self, ops::UVM_RE_FREE, &[a]);
                None
            }
            DpiBuiltin::ReCompExec | DpiBuiltin::ReCompExecFree => {
                // Selectors of the output are evaluated before the call, so
                // a host request they make cannot separate the result from
                // its use.
                let target = self.builtin_output_target(name, &args[3])?;
                let re = operand(self, 0)?;
                let string = operand(self, 1)?;
                let deglob = operand(self, 2)?;
                let code = if builtin == DpiBuiltin::ReCompExec {
                    ops::UVM_RE_COMPEXEC
                } else {
                    ops::UVM_RE_COMPEXECFREE
                };
                let result = call(self, code, &[re, string, deglob]);
                // Publishing the output may suspend the process, which keeps
                // no register: the result waits in a variable.
                let (spilled, _) = self.m.temp("dpi_result", 64, false, false);
                self.store(spilled, SIROffset::Static(0), 64, result);
                let exec_ret = call(self, ops::UVM_RE_LAST_EXEC, &[]);
                self.builtin_output(&target, exec_ret, 32, true)?;
                let result = self.b.alloc_bit(64, false);
                // From the region the store wrote.
                self.b.emit(SIRInstruction::Load(
                    result,
                    self.target(spilled),
                    SIROffset::Static(0),
                    64,
                ));
                Some(result)
            }
            DpiBuiltin::ReBuffer => Some(call(self, ops::UVM_RE_BUFFER, &[])),
            DpiBuiltin::NextArg => {
                let init = operand(self, 0)?;
                Some(call(self, ops::UVM_NEXT_ARG, &[init]))
            }
            DpiBuiltin::ToolName => Some(call(self, ops::UVM_TOOL_NAME, &[])),
            DpiBuiltin::ToolVersion => Some(call(self, ops::UVM_TOOL_VERSION, &[])),
            // `null`, 0 or "" (an empty string has handle 0): the facility
            // is unavailable; outputs keep their values, though their
            // selectors are evaluated as for any call.
            DpiBuiltin::Unavailable => {
                for (argument, actual) in import.arguments().iter().zip(args) {
                    if argument.is_output() && actual.is_some() {
                        self.builtin_output_target(name, actual)?;
                    }
                }
                import.return_type().map(|_| self.constant64(0))
            }
        };
        Ok(match (result, import.return_type()) {
            (Some(result), Some(r#type)) => {
                let width = r#type.width();
                let value = self.b.alloc_bit(width, false);
                self.b.emit(SIRInstruction::Slice(value, result, 0, width));
                Some(value)
            }
            _ => None,
        })
    }

    /// A string argument as a string: a string expression, or the bytes of
    /// an integral one such as a literal (IEEE 1800-2023 6.16).
    fn string_argument(&mut self, arg: &sv::ir::Expr) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let arg = self.hoist(arg)?;
        if self.m.expr_is_string(&arg) {
            return self.heap_operand(
                &arg,
                Some((32, false)),
                Some(celox_design::ValueKind::StrRef),
            );
        }
        self.string_call(StringOp::FromBits, &[Some(arg)])
    }

    /// The target of the output actual `actual` of builtin `name`. The
    /// tagged union members it accesses are checked here, as for any output
    /// actual; a builtin writes no variable that could change their tags.
    fn builtin_output_target(
        &mut self,
        name: &str,
        actual: &Option<sv::ir::Expr>,
    ) -> Result<BuiltinOutput, sv::AnalyzerError> {
        let actual = actual
            .as_ref()
            .ok_or_else(|| unsupported(format!("omitted output of DPI-C function `{name}`")))?;
        // A reference formal bound to a class property, forwarded: its
        // object was evaluated when the reference was made.
        if forwarded_property(actual, sv::ir::Expr::Literal("0".into())).is_some() {
            return Ok(BuiltinOutput::Property(actual.clone()));
        }
        let lvalues = lvalue_from_expr(actual)
            .ok_or_else(|| unsupported(format!("output argument of DPI-C function `{name}`")))?;
        self.check_tagged_guards(&tagged_actual_guards(actual))?;
        let hoisted = lvalues
            .iter()
            .map(|lvalue| {
                let lvalue = self.propagate_lvalue(lvalue);
                self.hoist_lvalue(&lvalue)
            })
            .collect::<Result<_, _>>()?;
        Ok(BuiltinOutput::Variables { lvalues, hoisted })
    }

    /// Write `value` to output target `target` (see
    /// [`Self::builtin_output_target`]).
    fn builtin_output(
        &mut self,
        target: &BuiltinOutput,
        value: RegisterId,
        width: usize,
        signed: bool,
    ) -> Result<(), sv::AnalyzerError> {
        let narrowed = self.b.alloc_bit(width, signed);
        self.b
            .emit(SIRInstruction::Slice(narrowed, value, 0, width));
        let (temp, temp_name) = self.m.temp("dpi_output", width, signed, false);
        self.store(temp, SIROffset::Static(0), width, narrowed);
        let value = sv::ir::Expr::Ident(temp_name);
        match target {
            BuiltinOutput::Variables { lvalues, hoisted } => {
                match hoisted.as_slice() {
                    [lvalue] => self.assign(lvalue, &value)?,
                    parts => self.assign_concat(parts, &value)?,
                }
                self.observe_assignment(lvalues)
            }
            BuiltinOutput::Property(actual) => {
                let store = forwarded_property(actual, value).expect("a forwarded property");
                self.exec_block(&[store]).map(drop)
            }
        }
    }
}

/// Where a builtin writes an output.
enum BuiltinOutput {
    /// The targets of the actual, and those targets with their selectors
    /// evaluated.
    Variables {
        lvalues: Vec<sv::ir::LValue>,
        hoisted: Vec<sv::ir::LValue>,
    },
    /// The class property a reference formal forwards (see
    /// [`forwarded_property`]).
    Property(sv::ir::Expr),
}
