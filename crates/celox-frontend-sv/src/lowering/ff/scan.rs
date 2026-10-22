use super::*;
use celox_design::{HostRequestKind, HostRequestSite, HostRequestValue};

impl Ff<'_, '_> {
    pub(super) fn scan_function(
        &mut self,
        function: sv::ir::SystemFunction,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::Expr;
        if self.kernel.is_none() {
            return Err(unsupported(format!(
                "{} requires Simulation scheduled execution",
                function.name()
            )));
        }
        let mut inputs = Vec::new();
        for (position, argument) in args.iter().take(2).enumerate() {
            let argument = argument
                .as_ref()
                .ok_or_else(|| unsupported("omitted scan source/format"))?;
            let argument = self.hoist(argument)?;
            let width = self
                .m
                .comparison_width(&argument, &argument)
                .ok_or_else(|| {
                    unsupported("scan source/format must be a packed integral or literal string")
                })?;
            // File descriptors are 32-bit packed values (21.3.1), even when
            // the actual expression has a wider integral type.
            let width = if function == sv::ir::SystemFunction::Fscanf && position == 0 {
                32
            } else {
                width
            };
            let (id, _) = self.host_hidden("scan_input", &argument, width, false, true)?;
            inputs.push(HostRequestValue {
                float_precision: None,
                id,
                width,
                signed: false,
                is_string: function != sv::ir::SystemFunction::Fscanf || position != 0,
            });
        }
        let (result_id, result_name) = self.m.temp("scan_count", 32, true, false);
        let mut results = vec![HostRequestValue {
            float_precision: None,
            id: result_id,
            width: 32,
            signed: true,
            is_string: false,
        }];
        let mut destinations = Vec::new();
        for argument in args.iter().skip(2) {
            let argument = argument
                .as_ref()
                .ok_or_else(|| unsupported("omitted scan destination"))?;
            let (lvalues, _) = self.capture_system_actual(argument)?;
            let width=self.m.comparison_width(argument,argument).ok_or_else(||unsupported("scan destination must be an integral variable; string/real outputs are unsupported"))?;
            let (id, name) = self.m.temp("scan_value", width, false, true);
            results.push(HostRequestValue {
                float_precision: None,
                id,
                width,
                signed: false,
                is_string: false,
            });
            destinations.push((lvalues, Expr::Ident(name)));
        }
        let site = self
            .host_request_sites
            .iter()
            .filter(|site| site.kind == HostRequestKind::Scan)
            .count() as u32;
        self.host_request_sites.push(HostRequestSite {
            process: 0,
            kind: HostRequestKind::Scan,
            operation: u32::from(function == sv::ir::SystemFunction::Fscanf),
            args: inputs,
            results,
            strings: Vec::new(),
        });
        self.with_kernel(|kernel| kernel.host_request(HostRequestKind::Scan, site))
            .map_err(kernel_error)?;
        // A failed or ignored output must not overwrite a previously assigned
        // alias. Scan assignments are contiguous even with suppressed fields.
        for (index, (lvalues, value)) in destinations.into_iter().enumerate() {
            let condition = Expr::Binary {
                left: Box::new(Expr::Ident(result_name.clone())),
                op: sv::ir::BinaryOp::Gt,
                right: Box::new(Expr::Literal(format!("32'sd{index}"))),
            };
            let (condition, _) = self.eval_truth(&condition)?;
            let write = self.b.new_block();
            let join = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: condition,
                true_block: (write, Vec::new()),
                false_block: (join, Vec::new()),
            });
            self.b.switch_to_block(write);
            match lvalues.as_slice() {
                [one] => self.assign(one, &value)?,
                parts => self.assign_concat(parts, &value)?,
            };
            self.jump(join);
            self.b.switch_to_block(join);
        }
        Ok(Expr::Ident(result_name))
    }
}
