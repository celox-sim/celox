//! Bypass packing when a single consumer works directly on whole fields.
use crate::HashMap;
use crate::ir::*;

/// Return the selected fields when both edges land on operand boundaries.
fn selected_fields<'a>(
    args: &'a [RegisterId],
    register_map: &HashMap<RegisterId, RegisterType>,
    offset: usize,
    width: usize,
) -> Option<&'a [RegisterId]> {
    let end = offset.checked_add(width)?;
    let mut start = 0usize;
    let mut low = None;
    for (i, arg) in args.iter().enumerate().rev() {
        if start == offset {
            low = Some(i);
        }
        start = start.checked_add(register_map.get(arg)?.width())?;
        if start >= end {
            let low = low?;
            let selected = &args[i..=low];
            return (start == end && selected.len() > 1 && selected.len() < args.len())
                .then_some(selected);
        }
    }
    None
}

pub(in crate::optimizer) fn fold_private_concat_consumers(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
) -> bool {
    if four_state {
        return false;
    }
    let mut packs: HashMap<_, _> = eu
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .filter_map(|inst| match inst {
            SIRInstruction::Concat(dst, args) if args.len() >= 2 => {
                Some((*dst, (args.clone(), 0usize)))
            }
            _ => None,
        })
        .collect();
    if packs.is_empty() {
        return false;
    }
    let mut note_use = |reg| {
        if let Some((_, uses)) = packs.get_mut(&reg) {
            *uses += 1;
        }
    };
    for block in eu.blocks.values() {
        for inst in &block.instructions {
            inst.for_each_use(&mut note_use);
        }
        block.terminator.for_each_use(&mut note_use);
    }
    let mut changed = false;
    for block in eu.blocks.values_mut() {
        for inst in &mut block.instructions {
            *inst = match *inst {
                SIRInstruction::Slice(dst, packed, offset, width) => {
                    let Some((args, 1)) = packs.get(&packed) else {
                        continue;
                    };
                    let Some(fields) = selected_fields(args, &eu.register_map, offset, width)
                    else {
                        continue;
                    };
                    SIRInstruction::Concat(dst, fields.to_vec())
                }
                SIRInstruction::Unary(dst, reduction, packed) => {
                    let Some((args, 1)) = packs.get(&packed) else {
                        continue;
                    };
                    let [a, b] = args[..] else { continue };
                    if eu.register_map[&a].width() != 1 || eu.register_map[&b].width() != 1 {
                        continue;
                    }
                    let op = match reduction {
                        UnaryOp::And => BinaryOp::And,
                        UnaryOp::Or => BinaryOp::Or,
                        UnaryOp::Xor => BinaryOp::Xor,
                        _ => continue,
                    };
                    SIRInstruction::Binary(dst, a, op, b)
                }
                _ => continue,
            };
            changed = true;
        }
    }
    changed
}
