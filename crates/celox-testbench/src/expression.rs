//! A binding to an expression compiled as an ordinary SIR kernel.
//! Machine code is owned by the simulator backend and its program image.
use crate::{ExprBytecode, ExprOpcode as Opcode, ExpressionKernel};

#[derive(Clone, Debug)]
pub struct CompiledExpr {
    bytecode: ExprBytecode,
    observes_hardware: bool,
}
impl CompiledExpr {
    pub fn new(bytecode: ExprBytecode) -> Self {
        Self {
            bytecode,
            observes_hardware: true,
        }
    }
    pub fn bytecode(&self) -> &ExprBytecode {
        &self.bytecode
    }
    pub fn kernel(&self) -> Option<&ExpressionKernel<usize>> {
        self.bytecode.kernel()
    }
    pub fn classify_hardware_access(mut self, mut hardware: impl FnMut(usize) -> bool) -> Self {
        self.observes_hardware = self.bytecode.ops().iter().any(|op| match op {
            Opcode::LoadU64 { location, .. }
            | Opcode::LoadWide { location, .. }
            | Opcode::LoadIndexed { location, .. }
            | Opcode::LoadArraySlice { location, .. }
            | Opcode::LoadBitSelect { location, .. }
            | Opcode::LoadString { location, .. }
            | Opcode::StoreString { location, .. }
            | Opcode::StoreU64 { location, .. } => hardware(*location),
            _ => false,
        });
        self
    }
    pub fn observes_hardware(&self) -> bool {
        self.observes_hardware
    }
    /// Literal folding for conservative binding-time overlap checks. General
    /// constant folding belongs to the shared SIR optimizer.
    pub fn constant_u64(&self) -> Option<u64> {
        self.kernel()
            .and_then(|kernel| kernel.constant)
            .or_else(|| self.bytecode.literal_u64())
    }
}

/// Portion of a selection that overlaps its base, in bits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SelectionWindow {
    pub offset: usize,
    pub skip: usize,
    pub width: usize,
}

impl SelectionWindow {
    /// Copy only the overlapping bits from a selected write.
    pub fn merge(
        self,
        previous: num_bigint::BigUint,
        value: num_bigint::BigUint,
    ) -> num_bigint::BigUint {
        if self.width == 0 {
            return previous;
        }
        let field = (num_bigint::BigUint::from(1u8) << self.width) - 1u8;
        let selected = field.clone() << self.offset;
        let preserved = &previous ^ (&previous & selected);
        preserved | (((value >> self.skip) & field) << self.offset)
    }
}

impl crate::TestbenchSelection<CompiledExpr> {
    pub fn window(&self, value: &crate::TestbenchValue, source_width: usize) -> SelectionWindow {
        if self.offset.bytecode().is_array_slice_window() {
            let value = value.to_biguint();
            let mut words = value.iter_u64_digits();
            let offset = usize::try_from(words.next().unwrap_or(0)).unwrap_or(usize::MAX);
            let skip = usize::try_from(words.next().unwrap_or(0)).unwrap_or(usize::MAX);
            let width = usize::try_from(words.next().unwrap_or(0)).unwrap_or(0);
            SelectionWindow {
                offset,
                skip,
                width: width
                    .min(source_width.saturating_sub(offset))
                    .min(self.width.saturating_sub(skip)),
            }
        } else {
            let offset = usize::try_from(value.to_u64()).unwrap_or(usize::MAX);
            SelectionWindow {
                offset,
                skip: 0,
                width: self.width.min(source_width.saturating_sub(offset)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TestbenchSelection, TestbenchValue};
    use num_bigint::BigUint;

    #[test]
    fn clipped_writes_merge_only_the_overlapping_bits() {
        let selection = TestbenchSelection {
            offset: CompiledExpr::new(ExprBytecode::new(vec![Opcode::ArraySliceWindow {
                size: 5,
                count: 2,
                stride: 8,
                subtract: 0,
                step: 1,
                signed: false,
            }])),
            width: 16,
        };
        let previous = BigUint::from(0x7766_5544_3322u64);
        let rhs = BigUint::from(0xbbaa_u16);
        for (offset, skip, width, expected) in [
            (32u64, 0u64, 8u64, 0x77aa_5544_3322u64),
            (0, 8, 8, 0x7766_5544_33bb),
            (0, 0, 0, 0x7766_5544_3322),
        ] {
            let window = TestbenchValue::Wide(
                (BigUint::from(width) << 128) | (BigUint::from(skip) << 64) | BigUint::from(offset),
            );
            let window = selection.window(&window, 48);
            assert_eq!(window.merge(previous.clone(), rhs.clone()), expected.into());
        }
    }
}
