//! Calls of DPI-C imports (IEEE 1800-2023 35.5).

use super::*;

impl Ff<'_, '_> {
    /// Call a DPI-C import, returning the register holding its result.
    ///
    /// Each argument is converted to its C type: a two-state integer, whose
    /// unknown bits read as zero, or a `logic`, which keeps its unknown bit
    /// in four-state simulation (IEEE 1800-2023 35.5.6). An import with an
    /// argument C takes through a pointer, or with a string, is called by
    /// [`Self::area_dpi_call`].
    pub(super) fn dpi_call(
        &mut self,
        import: &sv::ir::DpiImport,
        args: &[Option<sv::ir::Expr>],
        arg_order: &[usize],
    ) -> Result<Option<RegisterId>, sv::AnalyzerError> {
        let name = import.name();
        if args.len() != import.arguments().len() {
            return Err(unsupported(format!(
                "call of DPI-C function `{name}` with {} arguments; it takes {}",
                args.len(),
                import.arguments().len()
            )));
        }
        if let Some(builtin) = import.builtin() {
            // Keeps a process whose only effect is the call (see `Externs::calls`).
            self.m.externs.calls += 1;
            return self.builtin_dpi_call(import, builtin, args, arg_order);
        }
        if import.uses_call_area() {
            return self.area_dpi_call(import, args, arg_order);
        }
        let mut registers = vec![None; args.len()];
        let positions: Vec<_> = if arg_order.is_empty() {
            (0..args.len()).collect()
        } else {
            arg_order.to_vec()
        };
        // A register would not survive a host request in a later actual:
        // each actual before the last one that can make a request is kept, as
        // its C type, in a temporary until the call.
        let last_request = positions.iter().rposition(|&position| {
            args.get(position)
                .and_then(Option::as_ref)
                .is_some_and(|arg| self.m.makes_host_request(arg))
        });
        let mut kept = Vec::new();
        // Evaluate actuals in source order, then pass registers in C formal order.
        for (index, position) in positions.into_iter().enumerate() {
            let argument = import.arguments().get(position).ok_or_else(|| {
                unsupported(format!("argument position of DPI-C function `{name}`"))
            })?;
            let arg = &args[position];
            let Some(arg) = arg else {
                return Err(unsupported(format!(
                    "omitted argument of DPI-C function `{name}`"
                )));
            };
            let r#type = argument.r#type();
            let arg = self.hoist(arg)?;
            let value = self.eval(&arg, Some((r#type.width(), r#type.is_signed())))?;
            let register = self.dpi_register(value, r#type);
            if last_request.is_some_and(|last| index < last) {
                let four_state = matches!(self.b.register(&register), RegisterType::Logic { .. });
                let (temp, name) =
                    self.m
                        .temp("dpi_input", r#type.width(), r#type.is_signed(), four_state);
                self.store(temp, SIROffset::Static(0), r#type.width(), register);
                kept.push((position, name));
            } else {
                registers[position] = Some(register);
            }
        }
        for (position, name) in kept {
            let r#type = import.arguments()[position].r#type();
            let value = self.eval(
                &sv::ir::Expr::Ident(name),
                Some((r#type.width(), r#type.is_signed())),
            )?;
            registers[position] = Some(self.dpi_register(value, r#type));
        }
        let registers = registers
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| unsupported(format!("missing argument of DPI-C function `{name}`")))?;
        let func = self.m.extern_function(import);
        let dst = import
            .return_type()
            .map(|r#type| self.dpi_result_register(r#type));
        self.b.emit(SIRInstruction::ExternCall {
            dst,
            func,
            args: registers,
        });
        self.m.externs.calls += 1;
        Ok(dst)
    }

    /// `value`, of the argument's width, in the register type passed as its
    /// C type.
    pub(super) fn dpi_register(
        &mut self,
        value: RegisterId,
        r#type: sv::ir::DpiType,
    ) -> RegisterId {
        let register = self.dpi_result_register(r#type);
        let op = if matches!(self.b.register(&register), RegisterType::Logic { .. }) {
            UnaryOp::Ident
        } else {
            UnaryOp::ToTwoState
        };
        self.b.emit(SIRInstruction::Unary(register, op, value));
        register
    }

    /// A register of the type that holds a DPI-C value of type `r#type`.
    pub(super) fn dpi_result_register(&mut self, r#type: sv::ir::DpiType) -> RegisterId {
        if r#type.is_4state() && self.m.four_state {
            self.b.alloc_logic(1)
        } else {
            self.b.alloc_bit(r#type.width(), r#type.is_signed())
        }
    }
}
