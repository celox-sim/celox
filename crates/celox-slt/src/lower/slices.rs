//! Slice lowering and register width conversion.

use super::*;

impl SLTToSIRLowerer {
    pub(super) fn slice_reg<A>(
        &self,
        builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        access: &BitAccess,
    ) -> RegisterId {
        let width = access.msb - access.lsb + 1;
        // One Slice in both modes: the backends lower it to a shift and a
        // mask, and in four-state it transports Z, which a bitwise AND would
        // turn into X (IEEE 1800 11.4.8).
        let dest = builder.alloc_logic(width);
        builder.emit(SIRInstruction::Slice(dest, reg, access.lsb, width));
        dest
    }

    pub(super) fn cast_reg_width<A>(
        &self,
        builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        width: usize,
    ) -> RegisterId {
        self.cast_reg_width_ext(builder, reg, width, false)
    }

    pub(super) fn cast_reg_width_ext<A>(
        &self,
        builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        width: usize,
        signed: bool,
    ) -> RegisterId {
        let source_type = builder.register(&reg).clone();
        let current_width = source_type.width();
        let alloc_like_source = |builder: &mut SIRBuilder<A>, width, signed| match &source_type {
            RegisterType::Logic { .. } => builder.alloc_logic(width),
            RegisterType::Bit { .. } => builder.alloc_bit(width, signed),
        };
        if current_width == width {
            return reg;
        }
        if current_width < width {
            let pad_width = width - current_width;
            let pad = if signed {
                let sign = self.slice_reg(
                    builder,
                    reg,
                    &BitAccess::new(current_width - 1, current_width - 1),
                );
                if pad_width == 1 {
                    sign
                } else {
                    let ext = alloc_like_source(builder, pad_width, true);
                    builder.emit(SIRInstruction::Concat(
                        ext,
                        std::iter::repeat_n(sign, pad_width).collect(),
                    ));
                    ext
                }
            } else {
                let zero = builder.alloc_bit(pad_width, false);
                builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
                zero
            };
            let dest = alloc_like_source(builder, width, signed);
            builder.emit(SIRInstruction::Concat(dest, vec![pad, reg]));
            return dest;
        }

        let dest = alloc_like_source(builder, width, signed);
        // Truncation preserves the selected four-state bits, including Z.
        builder.emit(SIRInstruction::Slice(dest, reg, 0, width));
        dest
    }
}
