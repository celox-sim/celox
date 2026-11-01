//! Streaming concatenations of dynamically sized data (IEEE 1800-2023
//! 11.4.14), lowered to calls of the heap helper on a heap stream.
use super::*;
use celox_heap::{ops::encode, streams as heap};
use sv::ir::{StreamItem, StreamOp, StreamRange};

/// The heap's code of range form `form`.
fn range_form(form: StreamRange) -> u64 {
    match form {
        StreamRange::Up => 0,
        StreamRange::Bounds => 1,
        StreamRange::Down => 2,
    }
}

impl Ff<'_, '_> {
    /// The value of heap stream operation `op`, as a temporary.
    pub(super) fn stream_call(
        &mut self,
        op: &StreamOp,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let mut args = args
            .iter()
            .map(|arg| {
                arg.clone()
                    .ok_or_else(|| unsupported("omitted streaming concatenation operand"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        // The heap calls below keep the stream in a register: evaluate the
        // operands first, so that none of their calls ends the block while it
        // is live.
        if args.iter().any(|arg| self.m.calls(arg)) {
            let contexts = self.stream_operand_types(op);
            for (arg, ((width, signed), kind)) in args.iter_mut().zip(contexts) {
                // A fixed-size operand keeps its self-determined type
                // (11.4.14.1).
                let signed = signed.unwrap_or_else(|| self.expr_signed(arg));
                *arg = self.heap_operand(arg, Some((width, signed)), kind)?;
            }
        }
        let (result, width, kind) = match op {
            StreamOp::Pack(pack) => {
                let mut stream = self.heap_call(heap::NEW, &[]);
                let mut operands = args.iter();
                let mut next = || operands.next().ok_or_else(|| unsupported("stream operand"));
                for item in &pack.items {
                    stream = match *item {
                        StreamItem::Bits(arg) => {
                            // An operand keeps its self-determined type
                            // (11.4.14.1).
                            let expr = next()?;
                            let signed = self.expr_signed(expr);
                            let value = self.eval(expr, Some((arg.width, signed)))?;
                            let (lsb, size) = arg.bits.unwrap_or((0, arg.width));
                            for (offset, width) in arg.elements(size) {
                                stream =
                                    self.append_stream_bits(stream, value, lsb + offset, width);
                            }
                            stream
                        }
                        StreamItem::String => {
                            let string = self.handle(next()?)?;
                            self.heap_call(heap::STRING, &[stream, string])
                        }
                        StreamItem::Container { layout, range } => {
                            let container = self.handle(next()?)?;
                            let mut parameter = self.container_parameter(layout);
                            let range = match range {
                                Some(form) => {
                                    let (first, second) = (next()?.clone(), next()?.clone());
                                    parameter = heap::range_parameter(parameter, range_form(form));
                                    self.stream_range(&first, &second)?
                                }
                                None => self.constant64(0),
                            };
                            self.heap_call(
                                encode(heap::CONTAINER, parameter),
                                &[stream, container, range],
                            )
                        }
                        StreamItem::Stream => {
                            let other = self.integer_of_width(next()?, 32, false)?;
                            self.heap_call(heap::APPEND, &[stream, other])
                        }
                    };
                }
                if let Some(limit) = pack.limit {
                    stream = self.heap_call(encode(heap::TRUNCATE, limit as u64), &[stream]);
                }
                if let Some(slice) = pack.slice {
                    let order = if pack.unorder {
                        heap::UNORDER
                    } else {
                        heap::REORDER
                    };
                    stream = self.heap_call(encode(order, slice as u64), &[stream]);
                }
                (stream, 32, None)
            }
            StreamOp::ToBits { width, exact } => {
                let stream = self.integer_of_width(&args[0], 32, false)?;
                let stream = self.heap_call(
                    encode(heap::CHECK, heap::check_parameter(*width as u64, *exact)),
                    &[stream],
                );
                let zero = self.constant64(0);
                let value = self.peek_stream(stream, zero, *width);
                self.heap_call(heap::FREE, &[stream]);
                return Ok(self.stream_temp(value, *width));
            }
            StreamOp::Peek(width) => {
                let stream = self.integer_of_width(&args[0], 32, false)?;
                let skip = self.integer_of_width(&args[1], 64, false)?;
                let value = self.peek_stream(stream, skip, *width);
                return Ok(self.stream_temp(value, *width));
            }
            StreamOp::ToContainer(layout) => {
                let stream = self.integer_of_width(&args[0], 32, false)?;
                let zero = self.constant64(0);
                let parameter = self.container_parameter(*layout);
                let container = self.heap_call(encode(heap::TAKE_REST, parameter), &[stream, zero]);
                self.heap_call(heap::FREE, &[stream]);
                (container, 32, Some(celox_design::ValueKind::ObjRef))
            }
            StreamOp::ToString => {
                let stream = self.integer_of_width(&args[0], 32, false)?;
                let zero = self.constant64(0);
                let string = self.heap_call(heap::TAKE_STRING, &[stream, zero]);
                self.heap_call(heap::FREE, &[stream]);
                (string, 32, Some(celox_design::ValueKind::StrRef))
            }
            StreamOp::Skip(count) => {
                let stream = self.integer_of_width(&args[0], 32, false)?;
                let count = self.constant64(*count as u64);
                (self.heap_call(heap::SKIP, &[stream, count]), 32, None)
            }
            StreamOp::TakeRest { layout, reserve } => {
                let stream = self.integer_of_width(&args[0], 32, false)?;
                let reserve = self.constant64(u64::try_from(*reserve).unwrap_or(u64::MAX));
                let parameter = self.container_parameter(*layout);
                (
                    self.heap_call(encode(heap::TAKE_REST, parameter), &[stream, reserve]),
                    32,
                    Some(celox_design::ValueKind::ObjRef),
                )
            }
            StreamOp::TakeRange(layout, form) => {
                let stream = self.integer_of_width(&args[0], 32, false)?;
                let container = self.handle(&args[1])?;
                let range = self.stream_range(&args[2], &args[3])?;
                let parameter =
                    heap::range_parameter(self.container_parameter(*layout), range_form(*form));
                (
                    self.heap_call(
                        encode(heap::TAKE_RANGE, parameter),
                        &[stream, container, range],
                    ),
                    32,
                    Some(celox_design::ValueKind::ObjRef),
                )
            }
            StreamOp::TakeString { reserve } => {
                let stream = self.integer_of_width(&args[0], 32, false)?;
                let reserve = self.constant64(u64::try_from(*reserve).unwrap_or(u64::MAX));
                (
                    self.heap_call(heap::TAKE_STRING, &[stream, reserve]),
                    32,
                    Some(celox_design::ValueKind::StrRef),
                )
            }
            StreamOp::Free => {
                let stream = self.integer_of_width(&args[0], 32, false)?;
                (self.heap_call(heap::FREE, &[stream]), 32, None)
            }
        };
        let value = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Slice(value, result, 0, width));
        let (temp, name) = self.m.temp("stream", width, false, false);
        if let Some(kind) = kind {
            self.m.variables.get_mut(&temp).unwrap().value_kind = kind;
        }
        self.store(temp, SIROffset::Static(0), width, value);
        Ok(sv::ir::Expr::Ident(name))
    }

    /// The heap's `range` operand of the operands of a `with` range, each a
    /// signed 32-bit integer.
    fn stream_range(
        &mut self,
        first: &sv::ir::Expr,
        second: &sv::ir::Expr,
    ) -> Result<RegisterId, sv::AnalyzerError> {
        // Each bound keeps its own signedness (11.4.14.4).
        let signed = self.expr_signed(first);
        let first = self.integer_of_width(first, 32, signed)?;
        // Keep the second operand's bits clear of a sign-extended first.
        let low = self.constant64(u64::from(u32::MAX));
        let first_bits = self.b.alloc_bit(64, false);
        self.b.emit(SIRInstruction::Binary(
            first_bits,
            first,
            BinaryOp::And,
            low,
        ));
        let first = first_bits;
        let signed = self.expr_signed(second);
        let second = self.integer_of_width(second, 32, signed)?;
        let shift = self.constant64(32);
        let high = self.b.alloc_bit(64, false);
        self.b
            .emit(SIRInstruction::Binary(high, second, BinaryOp::Shl, shift));
        let range = self.b.alloc_bit(64, false);
        self.b
            .emit(SIRInstruction::Binary(range, high, BinaryOp::Or, first));
        Ok(range)
    }

    /// The width, signedness and value kind each operand of `op` is
    /// materialized with; a fixed-size operand's signedness is its own.
    fn stream_operand_types(
        &self,
        op: &StreamOp,
    ) -> Vec<((usize, Option<bool>), Option<celox_design::ValueKind>)> {
        use celox_design::ValueKind::{ObjRef, StrRef};
        let integer = ((32, None), None);
        match op {
            StreamOp::Pack(pack) => pack
                .items
                .iter()
                .flat_map(|item| match *item {
                    StreamItem::Bits(arg) => vec![((arg.width, None), None)],
                    StreamItem::String => vec![((32, Some(false)), Some(StrRef))],
                    StreamItem::Container { range, .. } => {
                        let mut types = vec![((32, Some(false)), Some(ObjRef))];
                        if range.is_some() {
                            types.extend([integer, integer]);
                        }
                        types
                    }
                    StreamItem::Stream => vec![((32, Some(false)), None)],
                })
                .collect(),
            StreamOp::Peek(_) => vec![((32, Some(false)), None), ((64, Some(false)), None)],
            StreamOp::TakeRange(..) => vec![
                ((32, Some(false)), None),
                ((32, Some(false)), Some(ObjRef)),
                integer,
                integer,
            ],
            _ => vec![((32, Some(false)), None)],
        }
    }

    /// `stream` extended by bits `lsb..lsb + width` of `value`, most
    /// significant first, in calls of at most 64 bits.
    fn append_stream_bits(
        &mut self,
        mut stream: RegisterId,
        value: RegisterId,
        lsb: usize,
        width: usize,
    ) -> RegisterId {
        let four_state = matches!(self.b.register(&value), RegisterType::Logic { .. });
        let mut remaining = width;
        while remaining > 0 {
            let count = remaining.min(64);
            remaining -= count;
            let chunk = if four_state {
                self.b.alloc_logic(count)
            } else {
                self.b.alloc_bit(count, false)
            };
            self.b
                .emit(SIRInstruction::Slice(chunk, value, lsb + remaining, count));
            let (bits, mask) = self.container_split(chunk);
            stream = self.heap_call(encode(heap::BITS, count as u64), &[stream, bits, mask]);
        }
        stream
    }

    /// `width` unread bits of `stream` after the first `skip`, most
    /// significant first, with their unknown bits in four-state simulation.
    fn peek_stream(&mut self, stream: RegisterId, skip: RegisterId, width: usize) -> RegisterId {
        let four_state = self.m.four_state;
        let mut parts = Vec::new();
        let mut offset = 0;
        while offset < width {
            let count = (width - offset).min(64);
            let at = self.constant64(offset as u64);
            let start = self.b.alloc_bit(64, false);
            self.b
                .emit(SIRInstruction::Binary(start, skip, BinaryOp::Add, at));
            let plane = |this: &mut Self, mask| {
                this.heap_call(
                    encode(heap::PEEK, heap::peek_parameter(count as u32, mask)),
                    &[stream, start],
                )
            };
            let value = plane(self, false);
            let mask = if four_state {
                plane(self, true)
            } else {
                self.constant64(0)
            };
            parts.push(self.container_join(value, mask, count, four_state));
            offset += count;
        }
        if let [part] = parts.as_slice() {
            return *part;
        }
        let value = if four_state {
            self.b.alloc_logic(width)
        } else {
            self.b.alloc_bit(width, false)
        };
        self.b.emit(SIRInstruction::Concat(value, parts));
        value
    }

    /// A temporary holding `width`-bit `value`.
    fn stream_temp(&mut self, value: RegisterId, width: usize) -> sv::ir::Expr {
        let four_state = matches!(self.b.register(&value), RegisterType::Logic { .. });
        let (temp, name) = self.m.temp("stream", width, false, four_state);
        self.store(temp, SIROffset::Static(0), width, value);
        sv::ir::Expr::Ident(name)
    }
}
