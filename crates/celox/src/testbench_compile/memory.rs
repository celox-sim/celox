//! Preserve unpacked element addressing when consuming flattened TB offsets.
use crate::HashMap;
use crate::ir::{AbsoluteAddr, RuntimeDesign};
use celox_design::{BinaryOp, RegionedStateAddr, UnaryOp};
use celox_sir::{
    ExecutionUnit, RegisterId, RegisterType, SIRInstruction as Inst, SIROffset, SIRValue,
};

pub(super) fn normalize(unit: &mut ExecutionUnit<RegionedStateAddr>, design: &RuntimeDesign) {
    let elements: HashMap<AbsoluteAddr, usize> = design
        .state_objects
        .iter()
        .filter(|(_, info)| !info.array_dims.is_empty())
        .map(|(address, info)| {
            (
                *address,
                info.width / info.array_dims.iter().product::<usize>(),
            )
        })
        .collect();
    let mut next = unit.register_map.keys().map(|reg| reg.0).max().unwrap_or(0) + 1;
    let mut alloc = |width| {
        let register = RegisterId(next);
        next += 1;
        unit.register_map.insert(
            register,
            RegisterType::Bit {
                width,
                signed: false,
            },
        );
        register
    };
    for block in unit.blocks.values_mut() {
        let old = std::mem::take(&mut block.instructions);
        for instruction in old {
            let (address, offset, width) = match &instruction {
                Inst::Load(_, address, offset, width)
                | Inst::Store(address, offset, width, _, _, _) => {
                    (*address, offset.clone(), *width)
                }
                _ => {
                    block.instructions.push(instruction);
                    continue;
                }
            };
            let Some(&element_width) = elements.get(&address.absolute_addr()) else {
                block.instructions.push(instruction);
                continue;
            };
            let static_start = if let SIROffset::Static(start) = offset {
                Some(start)
            } else {
                None
            };
            let start = match offset {
                SIROffset::Dynamic(value) => value,
                SIROffset::Static(offset) => {
                    if width == 0 || offset / element_width == (offset + width - 1) / element_width
                    {
                        block.instructions.push(instruction);
                        continue;
                    }
                    let reg = alloc(64);
                    block
                        .instructions
                        .push(Inst::Imm(reg, SIRValue::new(offset as u64)));
                    reg
                }
                _ => {
                    block.instructions.push(instruction);
                    continue;
                }
            };
            let stride = alloc(64);
            block
                .instructions
                .push(Inst::Imm(stride, SIRValue::new(element_width as u64)));
            let mut parts = Vec::new();
            let mut bit = 0;
            while bit < width {
                // Source packed selections are confined to their unpacked
                // element; wider accesses select complete unpacked elements.
                let available = static_start.map_or(element_width, |start| {
                    element_width - (start + bit) % element_width
                });
                let count = available.min(width - bit);
                let offset = if bit == 0 {
                    start
                } else {
                    let add = alloc(64);
                    block
                        .instructions
                        .push(Inst::Imm(add, SIRValue::new(bit as u64)));
                    let sum = alloc(64);
                    block
                        .instructions
                        .push(Inst::Binary(sum, start, BinaryOp::Add, add));
                    sum
                };
                let index = alloc(64);
                block
                    .instructions
                    .push(Inst::Binary(index, offset, BinaryOp::DivU, stride));
                let within = alloc(64);
                block
                    .instructions
                    .push(Inst::Binary(within, offset, BinaryOp::RemU, stride));
                let offset = SIROffset::Element {
                    index,
                    element_width,
                    bit_offset: 0,
                    dynamic_bit_offset: Some(within),
                };
                let part = alloc(count);
                match &instruction {
                    Inst::Load(..) => {
                        block
                            .instructions
                            .push(Inst::Load(part, address, offset, count));
                        parts.push(part);
                    }
                    Inst::Store(_, _, _, value, triggers, probes) => {
                        block
                            .instructions
                            .push(Inst::Slice(part, *value, bit, count));
                        block.instructions.push(Inst::Store(
                            address,
                            offset,
                            count,
                            part,
                            triggers.clone(),
                            probes.clone(),
                        ));
                    }
                    _ => unreachable!(),
                }
                bit += count;
            }
            if let Inst::Load(result, ..) = instruction {
                let value = if parts.len() == 1 {
                    parts[0]
                } else {
                    let value = alloc(width);
                    parts.reverse();
                    block.instructions.push(Inst::Concat(value, parts));
                    value
                };
                block
                    .instructions
                    .push(Inst::Unary(result, UnaryOp::Ident, value));
            }
        }
    }
}
