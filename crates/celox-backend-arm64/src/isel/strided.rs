//! Expand logical aggregate accesses across padded array elements before ISel.
//!
//! Native merged-chain optimization runs after layout and can create fresh
//! PackedElements loads/stores. Their logical bits must not address padding.
use std::borrow::Cow;

use crate::{
    ExecutionUnit, MemoryLayout, RegionedAbsoluteAddr, RegisterId, RegisterType, SIRInstruction,
    SIROffset,
};

fn split_access(
    instruction: &SIRInstruction<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
) -> Option<(usize, usize, usize)> {
    let (address, offset, width) = match instruction {
        SIRInstruction::Load(_, address, offset, width)
        | SIRInstruction::Store(address, offset, width, ..) => (address, offset, *width),
        _ => return None,
    };
    let start = match offset {
        SIROffset::Static(start)
        | SIROffset::PackedElements {
            bit_offset: start, ..
        } => *start,
        _ => return None,
    };
    let array = layout.unpacked_arrays.get(&address.absolute_addr())?;
    (width != 0
        && array.element_stride * 8 != array.element_width
        && width > array.element_width - start % array.element_width)
        .then_some((start, width, array.element_width))
}

pub(super) fn expand_strided_accesses<'a>(
    unit: &'a ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
) -> Cow<'a, ExecutionUnit<RegionedAbsoluteAddr>> {
    if !unit
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .any(|instruction| split_access(instruction, layout).is_some())
    {
        return Cow::Borrowed(unit);
    }
    let mut unit = unit.clone();
    let mut next_register = unit
        .register_map
        .keys()
        .map(|register| register.0)
        .max()
        .unwrap_or(0)
        + 1;
    for block in unit.blocks.values_mut() {
        let mut instructions = Vec::new();
        for instruction in std::mem::take(&mut block.instructions) {
            let Some((start, width, element_width)) = split_access(&instruction, layout) else {
                instructions.push(instruction);
                continue;
            };
            let register = match &instruction {
                SIRInstruction::Load(register, ..)
                | SIRInstruction::Store(_, _, _, register, ..) => *register,
                _ => unreachable!(),
            };
            let source_type = unit.register_map[&register].clone();
            let mut copied = 0;
            let mut pieces = Vec::new();
            while copied < width {
                let position = start + copied;
                let count = (element_width - position % element_width).min(width - copied);
                let piece = RegisterId(next_register);
                next_register += 1;
                let piece_type = match source_type {
                    RegisterType::Logic { .. } => RegisterType::Logic { width: count },
                    RegisterType::Bit { .. } => RegisterType::Bit {
                        width: count,
                        signed: false,
                    },
                };
                unit.register_map.insert(piece, piece_type);
                match &instruction {
                    SIRInstruction::Load(_, address, ..) => {
                        instructions.push(SIRInstruction::Load(
                            piece,
                            *address,
                            SIROffset::Static(position),
                            count,
                        ));
                        pieces.push(piece);
                    }
                    SIRInstruction::Store(address, _, _, source, triggers, captures) => {
                        instructions.push(SIRInstruction::Slice(piece, *source, copied, count));
                        instructions.push(SIRInstruction::Store(
                            *address,
                            SIROffset::Static(position),
                            count,
                            piece,
                            triggers.clone(),
                            captures.clone(),
                        ));
                    }
                    _ => unreachable!(),
                }
                copied += count;
            }
            if let SIRInstruction::Load(destination, ..) = instruction {
                pieces.reverse();
                instructions.push(SIRInstruction::Concat(destination, pieces));
            }
        }
        block.instructions = instructions;
    }
    Cow::Owned(unit)
}
