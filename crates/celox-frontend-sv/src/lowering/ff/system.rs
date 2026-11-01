use super::*;
use celox_design::{HostRequestKind, HostRequestSite, HostRequestValue, ValueKind};

impl Ff<'_, '_> {
    pub(super) fn system_call(
        &mut self,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        if self.kernel.is_none() {
            return Err(unsupported("`$system` outside a scheduled process"));
        }
        let mut inputs = Vec::new();
        for arg in args {
            let arg = arg
                .as_ref()
                .ok_or_else(|| unsupported("omitted $system argument"))?;
            inputs.push(self.format_input(arg)?);
        }
        let (id, name) = self.m.temp("system_result", 32, true, false);
        let site = self
            .host_request_sites
            .iter()
            .filter(|site| site.kind == HostRequestKind::System)
            .count() as u32;
        self.host_request_sites.push(HostRequestSite {
            process: 0,
            kind: HostRequestKind::System,
            operation: 0,
            args: inputs,
            results: vec![HostRequestValue {
                id,
                width: 32,
                signed: true,
                is_string: false,
                value_kind: ValueKind::Bits,
                float_precision: None,
            }],
            strings: Vec::new(),
        });
        self.with_kernel(|kernel| kernel.host_request(HostRequestKind::System, site))
            .map_err(kernel_error)?;
        Ok(sv::ir::Expr::Ident(name))
    }
}
