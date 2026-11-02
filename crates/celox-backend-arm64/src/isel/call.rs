//! Calls of function units ([`SIRInstruction::Call`]).
//!
//! A call guards its frame and the call depth, stores its arguments into the
//! callee's frame, calls the callee through the extern address table, and
//! reads `dst` from the output slots. The frame lies `frame` bytes into the
//! call stack of the state image, so its slots are addressed from the state
//! base with the frame offset as the index. The guard and the call are extern
//! calls, which no memory access crosses, so the guard stays before the
//! argument stores and the output loads after the call.
//!
//! [`SIRInstruction::Call`]: crate::SIRInstruction::Call

use celox_sir::extern_abi::{native_function_call, native_function_guard_for};

use super::four_state::get_wide_mask_chunks;
use super::*;
use celox_sir::{FrameLayout, FrameSlot, FunctionId};

pub(super) fn lower_call(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    function: FunctionId,
    frame: RegisterId,
    args: &[(usize, RegisterId)],
    outputs: &[usize],
    dst: Option<RegisterId>,
) {
    let functions = ctx
        .functions
        .expect("a unit that calls functions is compiled with the program's functions");
    let stack = ctx
        .layout
        .call_stack
        .expect("the layout of a program that calls functions has a call stack");
    let callee = &functions.frames[function.0 as usize];
    let slot = functions
        .first_slot
        .checked_add(function.0)
        .expect("function slots fit u32");
    // The verifier keeps the frame register a `Bit` of at most 64 bits.
    let frame = ctx.reg_map.get(frame);
    let base = i32::try_from(stack.base_offset).expect("the call stack fits i32");
    let stack_end = stack.base_offset + stack.size + celox_state_layout::CALL_STACK_GUARD;
    let alias_range = MemoryAliasRange::new(base, stack_end - stack.base_offset);

    block.push(MInst::CallExtern {
        dst: None,
        func: native_function_guard_for(callee.size),
        args: vec![frame],
    });
    for &(position, arg) in args {
        store_argument(
            ctx,
            block,
            frame,
            base,
            alias_range,
            &callee.slots[position],
            arg,
        );
    }
    block.push(MInst::CallExtern {
        dst: None,
        func: native_function_call(slot),
        args: vec![frame],
    });
    let Some(dst) = dst else {
        return;
    };
    load_outputs(ctx, block, frame, base, alias_range, callee, outputs, dst);
}

/// Store `arg` into `slot` of the frame `frame` bytes after state offset
/// `base`: its value plane, and its mask plane (zero for a two-state value)
/// when the slot has one in a four-state build.
fn store_argument(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    frame: VReg,
    base: i32,
    alias_range: Option<MemoryAliasRange>,
    slot: &FrameSlot<RegionedAbsoluteAddr>,
    arg: RegisterId,
) {
    let offset = base + i32::try_from(slot.offset).expect("frames fit i32");
    let chunks = ctx
        .get_wide_chunks(&arg, block)
        .into_iter()
        .map(|(chunk, _)| chunk)
        .collect::<Vec<_>>();
    store_plane(ctx, block, frame, offset, alias_range, slot.width, &chunks);
    if ctx.four_state && slot.four_state {
        let masks = plane_masks(ctx, block, arg, slot.width);
        let plane = i32::try_from(slot.plane_size()).expect("frames fit i32");
        store_plane(
            ctx,
            block,
            frame,
            offset + plane,
            alias_range,
            slot.width,
            &masks,
        );
    }
}

/// The mask chunks of `reg` as a `width`-bit plane.
fn plane_masks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    reg: RegisterId,
    width: usize,
) -> Vec<VReg> {
    let chunks = ISelContext::num_chunks(width).max(1);
    if ctx.sir_width(&reg) > 64 {
        get_wide_mask_chunks(ctx, block, &reg, chunks)
    } else {
        let mut masks = vec![ctx.get_mask(reg, block)];
        for _ in 1..chunks {
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            masks.push(zero);
        }
        masks
    }
}

/// Store the `width`-bit plane of 64-bit `chunks` at `offset` from the state
/// base, indexed by `frame`, in exactly its bytes.
fn store_plane(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    frame: VReg,
    offset: i32,
    alias_range: Option<MemoryAliasRange>,
    width: usize,
    chunks: &[VReg],
) {
    let bytes = width.div_ceil(8);
    for (index, &chunk) in chunks.iter().enumerate().take(bytes.div_ceil(8)) {
        let chunk_bytes = (bytes - index * 8).min(8);
        let chunk_bits = (width - index * 64).min(64);
        // The padding bits of a plane are zero, as in every state object.
        let mut value = if chunk_bits < 64 {
            let masked = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked, chunk, (1u64 << chunk_bits) - 1);
            masked
        } else {
            chunk
        };
        let mut at = offset + index as i32 * 8;
        let mut left = chunk_bytes;
        while left > 0 {
            let size = match left {
                8.. => OpSize::S64,
                4..=7 => OpSize::S32,
                2..=3 => OpSize::S16,
                _ => OpSize::S8,
            };
            block.push(MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: at,
                index: frame,
                src: value,
                size,
                alias_range,
            });
            let stored = size.bytes() as usize;
            left -= stored;
            at += stored as i32;
            if left > 0 {
                let rest = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: rest,
                    src: value,
                    imm: (stored * 8) as u8,
                });
                value = rest;
            }
        }
    }
}

/// The 64-bit chunks of the `width`-bit plane at `offset` from the state
/// base, indexed by `frame`, read in exactly its bytes.
fn load_plane(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    frame: VReg,
    offset: i32,
    alias_range: Option<MemoryAliasRange>,
    width: usize,
) -> Vec<VReg> {
    let bytes = width.div_ceil(8);
    (0..bytes.div_ceil(8))
        .map(|index| {
            let chunk_bytes = (bytes - index * 8).min(8);
            let mut at = offset + index as i32 * 8;
            let mut read = 0;
            let mut value = None;
            while read < chunk_bytes {
                let size = match chunk_bytes - read {
                    8.. => OpSize::S64,
                    4..=7 => OpSize::S32,
                    2..=3 => OpSize::S16,
                    _ => OpSize::S8,
                };
                let part = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::LoadIndexed {
                    dst: part,
                    base: BaseReg::SimState,
                    offset: at,
                    index: frame,
                    scale: 1,
                    size,
                    alias_range,
                });
                value = Some(match value {
                    None => part,
                    Some(low) => {
                        let shifted = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShlImm {
                            dst: shifted,
                            src: part,
                            imm: (read * 8) as u8,
                        });
                        let joined = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: joined,
                            lhs: low,
                            rhs: shifted,
                        });
                        joined
                    }
                });
                read += size.bytes() as usize;
                at += size.bytes() as i32;
            }
            let value = value.expect("a plane chunk has a byte");
            // A plane's padding bits are zero; mask them anyway, so that the
            // result is canonical whatever the callee left there.
            let chunk_bits = (width - index * 64).min(64);
            if chunk_bits < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, value, (1u64 << chunk_bits) - 1);
                masked
            } else {
                value
            }
        })
        .collect()
}

/// Define `dst` as the concatenation of the `outputs` slots of the callee's
/// frame, the first in the least significant bits.
#[allow(clippy::too_many_arguments)]
fn load_outputs(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    frame: VReg,
    base: i32,
    alias_range: Option<MemoryAliasRange>,
    callee: &FrameLayout<RegionedAbsoluteAddr>,
    outputs: &[usize],
    dst: RegisterId,
) {
    let width = ctx.sir_width(&dst);
    let words = width.div_ceil(64).max(1);
    let four_state = ctx.four_state;
    let mut values = Vec::with_capacity(outputs.len());
    let mut masks = Vec::with_capacity(outputs.len());
    for &position in outputs {
        let slot = &callee.slots[position];
        let offset = base + i32::try_from(slot.offset).expect("frames fit i32");
        values.push((
            load_plane(ctx, block, frame, offset, alias_range, slot.width),
            slot.width,
        ));
        if four_state {
            let mask = if slot.four_state {
                let plane = i32::try_from(slot.plane_size()).expect("frames fit i32");
                load_plane(ctx, block, frame, offset + plane, alias_range, slot.width)
            } else {
                Vec::new()
            };
            masks.push((mask, slot.width));
        }
    }
    let values = concatenate(ctx, block, &values, words);
    let value_chunks = values
        .iter()
        .enumerate()
        .map(|(index, &chunk)| (chunk, (width - index * 64).min(64)))
        .collect::<Vec<_>>();
    if width > 64 {
        ctx.set_wide_chunks(dst, value_chunks);
    } else {
        ctx.reg_map.set(dst, values[0]);
        ctx.known_bits.insert(values[0], width);
    }
    if four_state {
        let masks = concatenate(ctx, block, &masks, words);
        ctx.set_mask(dst, masks[0]);
        if width > 64 {
            ctx.wide_masks.insert(
                dst,
                masks
                    .iter()
                    .enumerate()
                    .map(|(index, &chunk)| (chunk, (width - index * 64).min(64)))
                    .collect(),
            );
        }
    }
}

/// Or each `(chunks, width)` part, whose chunks have no bits set above its
/// width (an empty part is zero), into `words` 64-bit words, the first part
/// in the least significant bits.
fn concatenate(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    parts: &[(Vec<VReg>, usize)],
    words: usize,
) -> Vec<VReg> {
    let mut result: Vec<Option<VReg>> = vec![None; words];
    let mut or_into = |ctx: &mut ISelContext, block: &mut MBlock, word: usize, value: VReg| {
        result[word] = Some(match result[word] {
            None => value,
            Some(previous) => {
                let joined = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: joined,
                    lhs: previous,
                    rhs: value,
                });
                joined
            }
        });
    };
    let mut shift = 0;
    for (chunks, width) in parts {
        for (index, &chunk) in chunks.iter().enumerate() {
            let bit = shift + index * 64;
            let (word, offset) = (bit / 64, (bit % 64) as u8);
            if word >= words {
                break;
            }
            let low = if offset == 0 {
                chunk
            } else {
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShlImm {
                    dst: shifted,
                    src: chunk,
                    imm: offset,
                });
                shifted
            };
            or_into(ctx, block, word, low);
            if offset != 0 && word + 1 < words {
                let high = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: high,
                    src: chunk,
                    imm: 64 - offset,
                });
                or_into(ctx, block, word + 1, high);
            }
        }
        shift += width;
    }
    result
        .into_iter()
        .map(|word| {
            word.unwrap_or_else(|| {
                let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: zero,
                    value: 0,
                });
                zero
            })
        })
        .collect()
}
