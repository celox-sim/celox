//! Share byte-sized bit transposes across adjacent Concat columns.
//!
//! RTL often spells a one-hot word selector as a matrix transpose followed
//! by column reductions. Lowering every Slice/Concat independently expands
//! each bit into shifts and ORs. An 8x8 tile needs only three butterfly stages.
//! Value and unknown-mask planes are transposed independently, preserving Z.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct TileKey {
    sources: [RegisterId; 8],
    byte: usize,
}

pub(super) type TileCache = HashMap<TileKey, Option<(VReg, Option<VReg>)>>;

fn tile_key(
    arguments: &[RegisterId],
    block: &BasicBlock<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, usize>,
    types: &HashMap<RegisterId, RegisterType>,
) -> Option<(TileKey, usize)> {
    let mut sources = [RegisterId(0); 8];
    let mut column = None;
    for (index, argument) in arguments.iter().enumerate() {
        let SIRInstruction::Slice(_, source, offset, 1) =
            block.instructions[*definitions.get(argument)?]
        else {
            return None;
        };
        if column.is_some_and(|column| column != offset)
            || types[&source].width() > 64
            || (offset / 8 + 1) * 8 > types[&source].width()
        {
            return None;
        }
        column = Some(offset);
        sources[index] = source;
    }
    let column = column?;
    Some((
        TileKey {
            sources,
            byte: column / 8,
        },
        column % 8,
    ))
}

pub(super) fn plan_tiles(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, usize>,
    types: &HashMap<RegisterId, RegisterType>,
) -> TileCache {
    let mut columns = HashMap::<TileKey, u8>::default();
    for instruction in &block.instructions {
        let SIRInstruction::Concat(destination, arguments) = instruction else {
            continue;
        };
        if types[destination].width() > 64 || arguments.iter().any(|arg| types[arg].width() != 1) {
            continue;
        }
        let arguments: Vec<_> = arguments.iter().rev().copied().collect();
        for group in arguments.as_chunks::<8>().0.iter() {
            if let Some((key, column)) = tile_key(group, block, definitions, types) {
                *columns.entry(key).or_default() |= 1 << column;
            }
        }
    }
    // Do not compute a complete tile for an isolated gather. At least four
    // distinct output columns must share it to amortize the butterfly stages.
    columns
        .into_iter()
        .filter(|(_, columns)| columns.count_ones() >= 4)
        .map(|(key, _)| (key, None))
        .collect()
}

fn emit_tile(ctx: &mut ISelContext, block: &mut MBlock, key: TileKey, mask: bool) -> VReg {
    let mut bytes = Vec::with_capacity(8);
    for source in key.sources {
        let mut value = if mask {
            ctx.get_mask(source, block)
        } else {
            ctx.reg_map.get(source)
        };
        if key.byte != 0 {
            let shifted = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShrImm {
                dst: shifted,
                src: value,
                imm: (key.byte * 8) as u8,
            });
            value = shifted;
        }
        bytes.push((value, 8));
    }
    let mut value = lower_flat_concat_to_chunks(ctx, block, bytes, 64)[0].0;
    if ctx.use_bmi2 {
        return value;
    }
    for (shift, mask) in [
        (7, 0x00aa_00aa_00aa_00aa),
        (14, 0x0000_cccc_0000_cccc),
        (28, 0x0000_0000_f0f0_f0f0),
    ] {
        let shifted = ctx.alloc_vreg(SpillDesc::transient());
        let difference = ctx.alloc_vreg(SpillDesc::transient());
        let selected = ctx.alloc_vreg(SpillDesc::transient());
        let moved = ctx.alloc_vreg(SpillDesc::transient());
        let low = ctx.alloc_vreg(SpillDesc::transient());
        let result = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::ShrImm {
            dst: shifted,
            src: value,
            imm: shift,
        });
        block.push(MInst::Xor {
            dst: difference,
            lhs: value,
            rhs: shifted,
        });
        ctx.emit_and_imm(block, selected, difference, mask);
        block.push(MInst::ShlImm {
            dst: moved,
            src: selected,
            imm: shift,
        });
        block.push(MInst::Xor {
            dst: low,
            lhs: value,
            rhs: selected,
        });
        block.push(MInst::Xor {
            dst: result,
            lhs: low,
            rhs: moved,
        });
        value = result;
    }
    value
}

fn extract_byte(ctx: &mut ISelContext, block: &mut MBlock, value: VReg, column: usize) -> VReg {
    if ctx.use_bmi2 {
        let mask = ctx.alloc_vreg(SpillDesc::remat(0x0101_0101_0101_0101 << column));
        block.push(MInst::LoadImm {
            dst: mask,
            value: 0x0101_0101_0101_0101 << column,
        });
        let result = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Pext {
            dst: result,
            src: value,
            mask,
        });
        return result;
    }
    if column == 0 {
        return value;
    }
    let shifted = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShrImm {
        dst: shifted,
        src: value,
        imm: (column * 8) as u8,
    });
    shifted
}

pub(super) fn try_lower(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    destination: RegisterId,
    arguments: &[RegisterId],
    sir_block: &BasicBlock<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, usize>,
) -> bool {
    if ctx.transpose_tiles.is_empty()
        || ctx.sir_width(&destination) > 64
        || arguments.iter().any(|arg| ctx.sir_width(arg) != 1)
    {
        return false;
    }
    let arguments: Vec<_> = arguments.iter().rev().copied().collect();
    let groups: Vec<_> = arguments
        .as_chunks::<8>()
        .0
        .iter()
        .map(|group| {
            tile_key(group, sir_block, definitions, ctx.register_types)
                .filter(|(key, _)| ctx.transpose_tiles.contains_key(key))
        })
        .collect();
    if groups.iter().all(Option::is_none) {
        return false;
    }
    let mut values = Vec::new();
    let mut masks = Vec::new();
    let mut index = 0;
    while index < arguments.len() {
        if index.is_multiple_of(8)
            && let Some(Some((key, column))) = groups.get(index / 8)
        {
            let (value, mask) = if let Some(cached) = ctx.transpose_tiles[key] {
                cached
            } else {
                let value = emit_tile(ctx, block, *key, false);
                let mask = ctx.four_state.then(|| emit_tile(ctx, block, *key, true));
                ctx.transpose_tiles.insert(*key, Some((value, mask)));
                (value, mask)
            };
            values.push((extract_byte(ctx, block, value, *column), 8));
            if let Some(mask) = mask {
                masks.push((extract_byte(ctx, block, mask, *column), 8));
            }
            index += 8;
        } else {
            values.push((ctx.reg_map.get(arguments[index]), 1));
            if ctx.four_state {
                masks.push((ctx.get_mask(arguments[index], block), 1));
            }
            index += 1;
        }
    }
    let width = ctx.sir_width(&destination);
    let value = lower_flat_concat_to_chunks(ctx, block, values, width)[0].0;
    let output = ctx.reg_map.get(destination);
    ctx.emit_mov(block, output, value);
    if ctx.four_state {
        let mask = lower_flat_concat_to_chunks(ctx, block, masks, width)[0].0;
        ctx.set_mask(destination, mask);
    }
    true
}
