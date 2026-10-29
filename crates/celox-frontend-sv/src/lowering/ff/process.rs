//! Process methods use the shared scheduler on every execution backend.
use super::*;
use celox_design::{HostRequestKind, HostRequestSite, HostRequestValue, ProcessOp, ValueKind};
impl Ff<'_, '_> {
    pub(super) fn process_call(
        &mut self,
        op: ProcessOp,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        if self.kernel.is_none() {
            return Err(unsupported("process method outside a process kernel"));
        }
        // The pre-scan classifies earlier waits. Keep the frame invariant here
        // too, including calls materialized while binding argument defaults.
        self.process_control = true;
        self.forked = true;
        self.urandom_state();
        let function = self
            .active_calls
            .iter()
            .skip(self.timing_call_boundary)
            .any(|name| self.m.subroutine(name).is_some_and(|s| !s.is_task));
        if op == ProcessOp::Await && function {
            return Err(unsupported("process.await task called from a function"));
        }
        let mut values = Vec::new();
        for (index, arg) in args.iter().enumerate() {
            let arg = self.hoist(
                arg.as_ref()
                    .ok_or_else(|| unsupported("missing process argument"))?,
            )?;
            // Key counts are `int` formals (IEEE 1800-2023 15.3).
            let int = match op {
                ProcessOp::Srandom
                | ProcessOp::SemaphorePut
                | ProcessOp::SemaphoreGet
                | ProcessOp::SemaphoreTryGet => index == 1,
                ProcessOp::SemaphoreNew => true,
                _ => false,
            };
            let arg = if int { self.int_formal(arg) } else { arg };
            let string = op == ProcessOp::SetRandstate && index == 1;
            // A string formal converts packed values before the handle is
            // materialized, just like an ordinary subroutine string argument.
            let arg = if string && !self.m.expr_is_string(&arg) {
                self.string_call(sv::ir::StringOp::FromBits, &[Some(arg)])?
            } else {
                arg
            };
            let (id, _) = self.host_hidden("process_argument", &arg, 32, false, !string)?;
            if string {
                self.m.variables.get_mut(&id).unwrap().value_kind = ValueKind::StrRef;
            }
            values.push(HostRequestValue {
                id,
                width: 32,
                signed: false,
                is_string: false,
                value_kind: if string {
                    ValueKind::StrRef
                } else {
                    ValueKind::Bits
                },
                float_precision: None,
            });
        }
        let signed = matches!(op, ProcessOp::Status | ProcessOp::SemaphoreTryGet);
        let (id, name) = self.m.temp("process_result", 32, signed, false);
        let string = op == ProcessOp::GetRandstate;
        if string {
            self.m.variables.get_mut(&id).unwrap().value_kind = ValueKind::StrRef;
        }
        let index = self
            .host_request_sites
            .iter()
            .filter(|s| s.kind == HostRequestKind::Process)
            .count() as u32;
        self.host_request_sites.push(HostRequestSite {
            process: 0,
            kind: HostRequestKind::Process,
            operation: op as u32
                | if self.process_predicate { 0x100 } else { 0 }
                | if function { 0x200 } else { 0 },
            args: values,
            results: vec![HostRequestValue {
                id,
                width: 32,
                signed,
                is_string: false,
                value_kind: if string {
                    ValueKind::StrRef
                } else {
                    ValueKind::Bits
                },
                float_precision: None,
            }],
            strings: Vec::new(),
        });
        self.with_kernel(|k| k.host_request(HostRequestKind::Process, index))
            .map_err(kernel_error)?;
        // `triggered` is a one-bit value in its expression (IEEE 1800-2023
        // 15.5.3), whatever the width of the result the scheduler writes.
        if op == ProcessOp::Triggered {
            return Ok(sv::ir::Expr::Resize {
                expr: Box::new(sv::ir::Expr::Ident(name)),
                width: 1,
                signed: false,
            });
        }
        Ok(sv::ir::Expr::Ident(name))
    }
}
