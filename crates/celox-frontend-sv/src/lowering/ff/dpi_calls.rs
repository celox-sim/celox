//! Calls of DPI-C imports with arguments C takes through a pointer, or
//! with strings (IEEE 1800-2023 35.5.6, H.7.4, H.8.8).
//!
//! `output` and `inout` arguments and packed arrays are words of the heap's
//! call area (see `celox_heap::ops::DPI_AREA`): inputs are stored there
//! before the call and outputs loaded from it afterwards. A packed array of
//! `bit` is an array of `svBitVecVal` and one of `logic` an array of
//! `svLogicVecVal`, whose `aval` and `bval` are the value and mask planes; a
//! smaller type is the C type it is passed as by value. On the supported
//! little-endian hosts, a 64-bit word of the area is two such 32-bit words,
//! the less significant first. A string is a `const char *` to a copy the
//! heap keeps until the next call, and a string the function gives back is
//! copied before anything else runs.

use super::actuals::SystemActual;
use super::*;
use celox_heap::ops;
use sv::ir::DpiType;

/// The number of 64-bit words of the call area an argument of type `r#type`
/// passed through a pointer points at.
fn area_words(r#type: DpiType) -> usize {
    match r#type {
        DpiType::BitVector { width, .. } => width.div_ceil(64),
        DpiType::LogicVector { width, .. } => width.div_ceil(32),
        _ => 1,
    }
}

impl Ff<'_, '_> {
    /// Call `import`, which passes an argument through a pointer or takes or
    /// returns a string, returning the register holding its result.
    pub(super) fn area_dpi_call(
        &mut self,
        import: &sv::ir::DpiImport,
        args: &[Option<sv::ir::Expr>],
        arg_order: &[usize],
    ) -> Result<Option<RegisterId>, sv::AnalyzerError> {
        let name = import.name();
        let arguments = import.arguments();
        let positions: Vec<_> = if arg_order.is_empty() {
            (0..args.len()).collect()
        } else {
            arg_order.to_vec()
        };
        // Actuals in source order. An output or inout actual is captured as
        // the place it names, with its selectors evaluated once, before the
        // actuals after it (IEEE 1800-2023 13.5); an inout reads that place.
        // Each input waits in a temporary of the formal's type, which a host
        // request of a later actual leaves intact.
        let mut inputs = vec![None; args.len()];
        let mut targets: Vec<Option<(SystemActual, Vec<sv::ir::Expr>)>> =
            args.iter().map(|_| None).collect();
        for position in positions {
            let argument = arguments.get(position).ok_or_else(|| {
                unsupported(format!("argument position of DPI-C function `{name}`"))
            })?;
            let r#type = argument.r#type();
            let actual = args[position].as_ref().ok_or_else(|| {
                unsupported(format!("omitted argument of DPI-C function `{name}`"))
            })?;
            let read = if argument.is_output() {
                let (target, read, guards) = self.capture_system_actual(actual)?;
                targets[position] = Some((target, guards));
                read
            } else {
                actual.clone()
            };
            if argument.is_input() {
                inputs[position] = Some(if r#type == DpiType::String {
                    self.string_argument(&read)?
                } else {
                    self.heap_operand(&read, Some((r#type.width(), r#type.is_signed())), None)?
                });
            }
        }
        // The words of the area each pointer argument points at.
        let mut offsets = vec![None; args.len()];
        let mut words = 0;
        for (position, argument) in arguments.iter().enumerate() {
            if argument.is_output() || argument.r#type().is_vector() {
                offsets[position] = Some(words);
                words += area_words(argument.r#type());
            }
        }
        let count = self.constant64(words as u64);
        self.heap_call(ops::DPI_AREA, &[count]);
        let mut registers = vec![None; args.len()];
        for (position, argument) in arguments.iter().enumerate() {
            let r#type = argument.r#type();
            match (offsets[position], &inputs[position]) {
                (None, Some(input)) if r#type == DpiType::String => {
                    let handle = self.handle(input)?;
                    registers[position] = Some(self.heap_call(ops::DPI_C_STRING, &[handle]));
                }
                (None, Some(input)) => {
                    let value = self.eval(input, Some((r#type.width(), r#type.is_signed())))?;
                    registers[position] = Some(self.dpi_register(value, r#type));
                }
                (Some(offset), Some(input)) => self.store_area(offset, r#type, input)?,
                (Some(_), None) => {}
                (None, None) => unreachable!("an argument passed by value is an input"),
            }
        }
        if words > 0 {
            let base = self.heap_call(ops::DPI_POINTER, &[]);
            for (position, offset) in offsets.iter().enumerate() {
                let Some(offset) = *offset else { continue };
                let bytes = self.constant64(8 * offset as u64);
                let pointer = self.b.alloc_bit(64, false);
                self.b
                    .emit(SIRInstruction::Binary(pointer, base, BinaryOp::Add, bytes));
                registers[position] = Some(pointer);
            }
        }
        let registers = registers
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| unsupported(format!("missing argument of DPI-C function `{name}`")))?;
        let func = self.m.extern_function(import);
        let dst = import.return_type().map(|r#type| {
            if r#type == DpiType::String {
                // A `const char *`.
                self.b.alloc_bit(64, false)
            } else {
                self.dpi_result_register(r#type)
            }
        });
        self.b.emit(SIRInstruction::ExternCall {
            dst,
            func,
            args: registers,
        });
        self.m.externs.calls += 1;
        // Publishing an output may suspend the process, which keeps no
        // register: the result and every output wait in variables, read
        // from the area before anything else can start it again.
        let result = match (dst, import.return_type()) {
            (Some(dst), Some(r#type)) => {
                let width = r#type.width();
                let (value, kind) = if r#type == DpiType::String {
                    (self.string_from_c(dst), celox_design::ValueKind::StrRef)
                } else {
                    (dst, celox_design::ValueKind::Bits)
                };
                let four_state = matches!(self.b.register(&value), RegisterType::Logic { .. });
                let (temp, temp_name) =
                    self.m
                        .temp("dpi_result", width, r#type.is_signed(), four_state);
                self.m.variables.get_mut(&temp).unwrap().value_kind = kind;
                self.store(temp, SIROffset::Static(0), width, value);
                Some((temp_name, r#type))
            }
            _ => None,
        };
        let mut outputs = Vec::new();
        for (position, argument) in arguments.iter().enumerate() {
            if let (Some(offset), true) = (offsets[position], argument.is_output()) {
                outputs.push((position, self.load_area(offset, argument.r#type())));
            }
        }
        for (position, value) in outputs {
            let (target, guards) = targets[position].as_ref().expect("an output has a target");
            self.check_tagged_guards(guards)?;
            self.write_system_actual(target, &sv::ir::Expr::Ident(value))?;
            self.observe_assignment(&target.parts)?;
        }
        result
            .map(|(temp_name, r#type)| {
                self.eval(
                    &sv::ir::Expr::Ident(temp_name),
                    Some((r#type.width(), r#type.is_signed())),
                )
            })
            .transpose()
    }

    /// The string handle, as a register of a handle's width, of the C string
    /// at 64-bit `pointer`.
    fn string_from_c(&mut self, pointer: RegisterId) -> RegisterId {
        let handle = self.heap_call(ops::DPI_FROM_C_STRING, &[pointer]);
        let narrowed = self.b.alloc_bit(celox_design::HANDLE_WIDTH, false);
        self.b.emit(SIRInstruction::Slice(
            narrowed,
            handle,
            0,
            celox_design::HANDLE_WIDTH,
        ));
        narrowed
    }

    /// `value` of `width` bits, at most `to`, zero-extended to `to` bits.
    fn zero_extended(&mut self, value: RegisterId, width: usize, to: usize) -> RegisterId {
        if width == to {
            return value;
        }
        let zero = self.b.alloc_bit(to - width, false);
        self.b
            .emit(SIRInstruction::Imm(zero, SIRValue::new(BigUint::zero())));
        let extended = self.b.alloc_bit(to, false);
        self.b
            .emit(SIRInstruction::Concat(extended, vec![zero, value]));
        extended
    }

    /// Bits `lsb +: bits` of two-state `value`, zero-extended to `to` bits.
    fn area_field(&mut self, value: RegisterId, lsb: usize, bits: usize, to: usize) -> RegisterId {
        let field = self.b.alloc_bit(bits, false);
        self.b.emit(SIRInstruction::Slice(field, value, lsb, bits));
        self.zero_extended(field, bits, to)
    }

    /// The value and mask planes of `value` of `width` bits as two-state
    /// registers; a two-state value has no mask.
    fn area_planes(&mut self, value: RegisterId, width: usize) -> (RegisterId, Option<RegisterId>) {
        if matches!(self.b.register(&value), RegisterType::Logic { .. }) {
            let (value, mask) = self.split_wide_planes(value, width);
            (value, Some(mask))
        } else {
            let plain = self.b.alloc_bit(width, false);
            self.b
                .emit(SIRInstruction::Unary(plain, UnaryOp::Ident, value));
            (plain, None)
        }
    }

    /// Store `input`, the value of an argument of type `r#type`, in the
    /// words of the call area from `offset`.
    fn store_area(
        &mut self,
        offset: usize,
        r#type: DpiType,
        input: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        let width = r#type.width();
        let store = |this: &mut Self, index: usize, word: RegisterId| {
            let index = this.constant64((offset + index) as u64);
            this.heap_call(ops::DPI_STORE, &[index, word]);
        };
        match r#type {
            DpiType::String => {
                let handle = self.handle(input)?;
                let pointer = self.heap_call(ops::DPI_C_STRING, &[handle]);
                store(self, 0, pointer);
            }
            DpiType::Bit | DpiType::Integer { .. } | DpiType::Chandle => {
                let word = self.integer_of_width(input, width, r#type.is_signed())?;
                store(self, 0, word);
            }
            DpiType::Logic => {
                // An `svLogic`: `value | mask << 1`.
                let value = self.eval(input, Some((1, false)))?;
                let (value, mask) = self.area_planes(value, 1);
                let word = match mask {
                    Some(mask) => {
                        let zero = self.b.alloc_bit(62, false);
                        self.b
                            .emit(SIRInstruction::Imm(zero, SIRValue::new(BigUint::zero())));
                        let word = self.b.alloc_bit(64, false);
                        self.b
                            .emit(SIRInstruction::Concat(word, vec![zero, mask, value]));
                        word
                    }
                    None => self.zero_extended(value, 1, 64),
                };
                store(self, 0, word);
            }
            DpiType::BitVector { .. } => {
                let value = self.eval(input, Some((width, false)))?;
                let known = self.b.alloc_bit(width, false);
                self.b
                    .emit(SIRInstruction::Unary(known, UnaryOp::ToTwoState, value));
                for index in 0..area_words(r#type) {
                    let bits = (width - 64 * index).min(64);
                    let word = self.area_field(known, 64 * index, bits, 64);
                    store(self, index, word);
                }
            }
            DpiType::LogicVector { .. } => {
                let value = self.eval(input, Some((width, false)))?;
                let (value, mask) = self.area_planes(value, width);
                for index in 0..area_words(r#type) {
                    let bits = (width - 32 * index).min(32);
                    // `aval` in the low half and `bval` in the high half.
                    let word = match mask {
                        Some(mask) => {
                            let aval = self.area_field(value, 32 * index, bits, 32);
                            let bval = self.area_field(mask, 32 * index, bits, 32);
                            let word = self.b.alloc_bit(64, false);
                            self.b.emit(SIRInstruction::Concat(word, vec![bval, aval]));
                            word
                        }
                        None => self.area_field(value, 32 * index, bits, 64),
                    };
                    store(self, index, word);
                }
            }
        }
        Ok(())
    }

    /// The four-state value of `bits` bits with value plane `aval` and mask
    /// plane `bval`; in two-state simulation its unknown bits read as zero.
    fn area_logic(&mut self, aval: RegisterId, bval: RegisterId, bits: usize) -> RegisterId {
        if self.m.four_state {
            return self.join_wide_planes(aval, bval, bits);
        }
        let known = self.b.alloc_bit(bits, false);
        self.b
            .emit(SIRInstruction::Unary(known, UnaryOp::BitNot, bval));
        let value = self.b.alloc_bit(bits, false);
        self.b
            .emit(SIRInstruction::Binary(value, aval, BinaryOp::And, known));
        value
    }

    /// The name of a temporary holding the value of type `r#type` that the
    /// words of the call area from `offset` hold after the call.
    fn load_area(&mut self, offset: usize, r#type: DpiType) -> String {
        let width = r#type.width();
        let four_state = r#type.is_4state() && self.m.four_state;
        let (temp, name) = self
            .m
            .temp("dpi_output", width, r#type.is_signed(), four_state);
        let load = |this: &mut Self, index: usize| {
            let index = this.constant64((offset + index) as u64);
            this.heap_call(ops::DPI_LOAD, &[index])
        };
        let field = |this: &mut Self, word: RegisterId, lsb: usize, bits: usize| {
            let field = this.b.alloc_bit(bits, false);
            this.b.emit(SIRInstruction::Slice(field, word, lsb, bits));
            field
        };
        match r#type {
            DpiType::String => {
                let pointer = load(self, 0);
                let handle = self.string_from_c(pointer);
                self.m.variables.get_mut(&temp).unwrap().value_kind =
                    celox_design::ValueKind::StrRef;
                self.store(temp, SIROffset::Static(0), width, handle);
            }
            DpiType::Bit | DpiType::Integer { .. } | DpiType::Chandle => {
                let word = load(self, 0);
                let value = field(self, word, 0, width);
                self.store(temp, SIROffset::Static(0), width, value);
            }
            DpiType::Logic => {
                let word = load(self, 0);
                let aval = field(self, word, 0, 1);
                let bval = field(
                    self,
                    word,
                    celox_sir::extern_abi::SV_LOGIC_MASK_SHIFT as usize,
                    1,
                );
                let value = self.area_logic(aval, bval, 1);
                self.store(temp, SIROffset::Static(0), 1, value);
            }
            DpiType::BitVector { .. } => {
                for index in 0..area_words(r#type) {
                    let bits = (width - 64 * index).min(64);
                    let word = load(self, index);
                    let value = field(self, word, 0, bits);
                    self.store(temp, SIROffset::Static(64 * index), bits, value);
                }
            }
            DpiType::LogicVector { .. } => {
                for index in 0..area_words(r#type) {
                    let bits = (width - 32 * index).min(32);
                    let word = load(self, index);
                    let aval = field(self, word, 0, bits);
                    let bval = field(self, word, 32, bits);
                    let value = self.area_logic(aval, bval, bits);
                    self.store(temp, SIROffset::Static(32 * index), bits, value);
                }
            }
        }
        name
    }
}
