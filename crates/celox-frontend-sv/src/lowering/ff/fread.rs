use super::*;
use celox_design::HostRequestValue;

impl Ff<'_, '_> {
    pub(super) fn fread(
        &mut self,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let destination = args
            .first()
            .and_then(Option::as_ref)
            .ok_or_else(|| unsupported("$fread destination"))?;
        let memory = match destination {
            sv::ir::Expr::Ident(name) => self
                .m
                .id(name)
                .filter(|id| !self.m.var(*id).array_dims.is_empty()),
            _ => None,
        };
        let (left, right, element_width, width) = if let Some(id) = memory {
            if self.m.var(id).array_dims.len() != 1 {
                return Err(unsupported(
                    "$fread requires a one-dimensional static memory",
                ));
            }
            let (left, right, width) = self.m.file_memory_shape(id)?;
            (left, right, width, self.m.var(id).width)
        } else {
            let width = self
                .m
                .comparison_width(destination, destination)
                .ok_or_else(|| unsupported("packed integral $fread destination"))?;
            (0, 0, width, width)
        };
        // Evaluate the reference once, then all value operands, then snapshot
        // the referenced storage so partial reads retain argument side effects.
        let (lvalues, captured, guards) = self.capture_system_actual(destination)?;
        let mut values = Vec::new();
        for (position, arg) in args.iter().enumerate().skip(1) {
            if let Some(arg) = arg {
                let expr = sv::ir::Expr::Resize {
                    expr: Box::new(arg.clone()),
                    width: 32,
                    signed: self.expr_signed(arg),
                };
                let mut value = self.file_io_arg(&sv::ir::SystemTaskArg::Expr(expr))?;
                value.signed = position > 1;
                values.push(value);
            } else if position == 1 {
                return Err(unsupported("$fread file descriptor"));
            } else {
                // Missing addressing operands retain their positions in metadata.
                let literal = sv::ir::Expr::Literal("0".into());
                values.push(self.file_io_arg(&sv::ir::SystemTaskArg::Expr(literal))?);
            }
        }
        let (data_id, data) = self.host_hidden("fread_data", &captured, width, false, true)?;
        let (result_id, result_name) = self.m.temp("fread_count", 32, true, false);
        self.file_io_request(
            13,
            values,
            vec![
                HostRequestValue {
                    float_precision: None,
                    id: result_id,
                    width: 32,
                    signed: true,
                    is_string: false,
                },
                HostRequestValue {
                    float_precision: None,
                    id: data_id,
                    width,
                    signed: false,
                    is_string: false,
                },
            ],
            vec![
                left.to_string(),
                right.to_string(),
                element_width.to_string(),
                u8::from(memory.is_some()).to_string(),
                u8::from(args.get(2).is_some_and(Option::is_some)).to_string(),
                u8::from(args.get(3).is_some_and(Option::is_some)).to_string(),
            ],
        )?;
        self.check_tagged_guards(&guards)?;
        match lvalues.as_slice() {
            [target] => self.assign(target, &data)?,
            targets => self.assign_concat(targets, &data)?,
        }
        Ok(sv::ir::Expr::Ident(result_name))
    }
}
