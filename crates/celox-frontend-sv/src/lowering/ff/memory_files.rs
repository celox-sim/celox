//! `$readmemh` and `$readmemb` in imperative code: the file is read when the
//! task executes (IEEE 1800-2023 21.4).

use super::*;
use celox_frontend_core::memory_file::lowering::{lower_memory_file_load, message_present};

impl Ff<'_, '_> {
    /// `$readmemh`/`$readmemb`: the heap's memory-file reader reads the file
    /// when the task executes, and the words it gives are stored into the
    /// memory, whose other words keep their values. A warning or an error
    /// of the load is reported then; an error terminates the load, keeping
    /// the words loaded before it, and the simulation goes on.
    pub(super) fn readmem(
        &mut self,
        format: sv::ir::MemoryFormat,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
        site: Option<&sv::ir::SourceSpan>,
    ) -> Result<(), sv::AnalyzerError> {
        let task = sv::ir::SystemTask::Readmem(format);
        // The file name is evaluated first, then the addresses (IEEE
        // 1800-2023 21.4).
        let file = if constant_file_name(args.first()) {
            None
        } else {
            let Some(sv::ir::SystemTaskArg::Expr(name)) = args.first() else {
                return Err(unsupported(format!("`{task}` without a file name")));
            };
            let name = self.string_argument(name)?;
            Some(self.handle(&name)?)
        };
        let (id, load) = self.m.readmem_load(format, args, None, None, site)?;
        let (_, _, default_start, default_finish) = range_addresses(&load);
        let mut addresses = [None, None];
        let mut unknown = None;
        for (slot, arg) in addresses.iter_mut().zip(args.iter().skip(2)) {
            let sv::ir::SystemTaskArg::Expr(expr) = arg else {
                continue;
            };
            let (value, value_unknown) = self.address_operand(expr)?;
            *slot = Some(value);
            unknown = Some(match unknown {
                None => value_unknown,
                Some(previous) => {
                    let either = self.b.alloc_bit(64, false);
                    self.b.emit(SIRInstruction::Binary(
                        either,
                        previous,
                        BinaryOp::Or,
                        value_unknown,
                    ));
                    either
                }
            });
        }
        let range = match (addresses, unknown) {
            ([start, finish], Some(unknown)) => {
                let mut address = |given: Option<RegisterId>, default: i128| match given {
                    Some(register) => register,
                    None => self.constant64(default as i64 as u64),
                };
                Some((
                    address(start, default_start),
                    address(finish, default_finish),
                    unknown,
                ))
            }
            _ => None,
        };
        let element_width = load.element_width;
        if let Some((lsb, msb)) = load.written_bits() {
            self.writes
                .memory_loads
                .push((id, BitAccess::new(lsb, msb)));
        }
        let four_state = self.m.four_state && self.m.var(id).is_4state;
        let messages = lower_memory_file_load(
            self,
            |this| &mut this.b,
            &load,
            file,
            range,
            four_state,
            |this, position, word| {
                let offset = SIROffset::Element {
                    index: position,
                    element_width,
                    bit_offset: 0,
                    dynamic_bit_offset: None,
                };
                this.store(id, offset, element_width, word);
            },
        );
        for (severity, message) in [
            (celox_design::Severity::Warning, messages.warning),
            (celox_design::Severity::Error, messages.error),
        ] {
            self.report_message(severity, message, site)?;
        }
        Ok(())
    }

    /// Address `expr` as a signed 64-bit integer, with 1 in a 64-bit
    /// register when it has an unknown bit.
    fn address_operand(
        &mut self,
        expr: &sv::ir::Expr,
    ) -> Result<(RegisterId, RegisterId), sv::AnalyzerError> {
        let signed = self.expr_signed(expr);
        let expr = self.hoist(expr)?;
        let value = self.eval(&expr, Some((64, signed)))?;
        let unknown = self.b.alloc_bit(64, false);
        if let RegisterType::Logic { width } = self.b.register(&value).clone() {
            // The planes of whole 64-bit words.
            let (value, width) = if width.is_multiple_of(64) {
                (value, width)
            } else {
                let words = width.next_multiple_of(64);
                let widened = self.b.alloc_logic(words);
                self.b
                    .emit(SIRInstruction::Unary(widened, UnaryOp::Ident, value));
                (widened, words)
            };
            let planes = self.b.alloc_bit(2 * width, false);
            self.b
                .emit(SIRInstruction::Unary(planes, UnaryOp::ToPlanes, value));
            let mask = self.b.alloc_bit(width, false);
            self.b
                .emit(SIRInstruction::Slice(mask, planes, width, width));
            let zero = self.b.alloc_bit(width, false);
            self.b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
            let any = self.b.alloc_bit(1, false);
            self.b
                .emit(SIRInstruction::Binary(any, mask, BinaryOp::Ne, zero));
            self.b
                .emit(SIRInstruction::Unary(unknown, UnaryOp::Ident, any));
        } else {
            self.b
                .emit(SIRInstruction::Imm(unknown, SIRValue::new(0u64)));
        }
        let known = self.b.alloc_bit(64, signed);
        self.b
            .emit(SIRInstruction::Unary(known, UnaryOp::ToTwoState, value));
        let operand = self.b.alloc_bit(64, false);
        self.b
            .emit(SIRInstruction::Unary(operand, UnaryOp::Ident, known));
        Ok((operand, unknown))
    }

    /// Report string `message`, a 64-bit handle, at `severity` unless it is
    /// empty.
    fn report_message(
        &mut self,
        severity: celox_design::Severity,
        message: RegisterId,
        site: Option<&sv::ir::SourceSpan>,
    ) -> Result<(), sv::AnalyzerError> {
        let present = message_present(&mut self.b, message);
        let report = self.b.new_block();
        let done = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: present,
            true_block: (report, Vec::new()),
            false_block: (done, Vec::new()),
        });
        self.b.switch_to_block(report);
        let handle = self.b.alloc_bit(celox_design::HANDLE_WIDTH, false);
        self.b.emit(SIRInstruction::Slice(
            handle,
            message,
            0,
            celox_design::HANDLE_WIDTH,
        ));
        // A report carries the time it is issued at (IEEE 1800-2023 20.10).
        let time = sv::ir::Expr::Ident(self.m.time_slot());
        let time = self.eval(&time, None)?;
        let time_width = self.b.register(&time).width();
        let site_id = self.m.event_site(RuntimeEventSite {
            kind: RuntimeEventKind::Report(severity),
            template: Some("%s".to_string()),
            sizing: DisplaySizing::Ieee,
            scope: None,
            location: report_location(site),
            assertion: None,
            deferred: None,
            timestamp_arg: true,
            time_format_arg: false,
            arg_widths: vec![celox_design::HANDLE_WIDTH, time_width],
            arg_signed: vec![false, false],
            arg_is_string: vec![false, false],
            arg_float_precision: vec![None, None],
            arg_is_handle: vec![true, false],
        });
        self.b.emit(SIRInstruction::RuntimeEvent {
            site_id,
            args: vec![handle, time],
        });
        self.b.seal_block(SIRTerminator::Jump(done, Vec::new()));
        self.b.switch_to_block(done);
        Ok(())
    }
}
