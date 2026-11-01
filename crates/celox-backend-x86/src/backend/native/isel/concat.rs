//! Concatenation, guarded selections, and mux chunk blending.

use super::*;

pub(super) fn match_guarded_cmp_select_cond(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    sir_block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    sir_defs: &HashMap<RegisterId, usize>,
    cond: RegisterId,
) -> Option<(VReg, VReg, VReg, CmpKind)> {
    let &cond_idx = sir_defs.get(&cond)?;
    let SIRInstruction::Binary(_, lhs, BinaryOp::LogicAnd, rhs) = sir_block.instructions[cond_idx]
    else {
        return None;
    };
    if let Some((cmp_lhs, cmp_rhs, kind)) =
        match_cmp_sir_value(ctx, block, sir_block, sir_defs, lhs)
    {
        let guard = lower_sir_bool_value(ctx, block, rhs)?;
        return Some((guard, cmp_lhs, cmp_rhs, kind));
    }
    if let Some((cmp_lhs, cmp_rhs, kind)) =
        match_cmp_sir_value(ctx, block, sir_block, sir_defs, rhs)
    {
        let guard = lower_sir_bool_value(ctx, block, lhs)?;
        return Some((guard, cmp_lhs, cmp_rhs, kind));
    }
    None
}

fn match_cmp_sir_value(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    sir_block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    sir_defs: &HashMap<RegisterId, usize>,
    reg: RegisterId,
) -> Option<(VReg, VReg, CmpKind)> {
    let &idx = sir_defs.get(&reg)?;
    let SIRInstruction::Binary(_, lhs, op, rhs) = sir_block.instructions[idx] else {
        return None;
    };
    let kind = match op {
        BinaryOp::Eq | BinaryOp::EqWildcard => CmpKind::Eq,
        BinaryOp::Ne | BinaryOp::NeWildcard => CmpKind::Ne,
        BinaryOp::LtU => CmpKind::LtU,
        BinaryOp::LtS => CmpKind::LtS,
        BinaryOp::LeU => CmpKind::LeU,
        BinaryOp::LeS => CmpKind::LeS,
        BinaryOp::GtU => CmpKind::GtU,
        BinaryOp::GtS => CmpKind::GtS,
        BinaryOp::GeU => CmpKind::GeU,
        BinaryOp::GeS => CmpKind::GeS,
        _ => return None,
    };
    if ctx.sir_width(&lhs) > 64
        || ctx.sir_width(&rhs) > 64
        || ctx.wide_regs.contains_key(&lhs)
        || ctx.wide_regs.contains_key(&rhs)
    {
        return None;
    }
    let lhs_vreg = ctx.reg_map.get(lhs);
    let rhs_vreg = ctx.reg_map.get(rhs);
    // The fused compare must use the same signed operands as an ordinary
    // comparison. SIR values narrower than a machine word are zero extended.
    let (lhs_vreg, rhs_vreg) = if matches!(
        kind,
        CmpKind::LtS | CmpKind::LeS | CmpKind::GtS | CmpKind::GeS
    ) {
        sign_extend_pair(ctx, block, &lhs, &rhs, lhs_vreg, rhs_vreg)
    } else {
        (lhs_vreg, rhs_vreg)
    };
    Some((lhs_vreg, rhs_vreg, kind))
}

fn lower_sir_bool_value(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    reg: RegisterId,
) -> Option<VReg> {
    if ctx.sir_width(&reg) > 64 {
        return None;
    }
    let raw = if ctx.wide_regs.contains_key(&reg) {
        ctx.get_wide_chunks(&reg, block)[0].0
    } else {
        ctx.reg_map.get(reg)
    };
    Some(lower_bool_value(ctx, block, raw))
}

/// Lower an at-most-machine-word concat whose high part is a repeated one-bit
/// value.
///
/// HDL sign extension commonly reaches SIR as
///
/// ```text
/// Concat([sign, sign, ..., sign, low_bits])
/// ```
///
/// Expanding that literally emits one shift and one OR per repeated bit. A
/// one-bit value is exactly zero or one in each value/mask plane, so negating
/// it creates the required all-zero/all-one fill word. Masking or shifting
/// that fill into the high part and ORing the low value implements the complete
/// concat with constant work. Wider values use the normal chunk lowering.
pub(super) fn try_lower_repeated_msb_concat(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    args: &[RegisterId],
) -> bool {
    const MIN_REPEATED_BITS: usize = 4;

    let result_width = ctx.sir_width(&dst);
    if result_width > 64 || args.len() <= MIN_REPEATED_BITS {
        return false;
    }

    let repeated = args[0];
    let suffix = *args.last().expect("non-empty concat");
    let repeated_bits = args.len() - 1;
    let suffix_width = ctx.sir_width(&suffix);
    if ctx.sir_width(&repeated) != 1
        || repeated_bits < MIN_REPEATED_BITS
        || repeated_bits + suffix_width != result_width
        || !args[..repeated_bits]
            .iter()
            .all(|candidate| *candidate == repeated)
    {
        return false;
    }

    fn lower_plane(
        ctx: &mut ISelContext,
        block: &mut MBlock,
        repeated: VReg,
        suffix: VReg,
        suffix_width: usize,
        result_width: usize,
        destination: Option<VReg>,
    ) -> VReg {
        if repeated == suffix && suffix_width == 1 {
            let result = destination.unwrap_or_else(|| ctx.alloc_vreg(SpillDesc::transient()));
            let fill = if result_width == 64 {
                result
            } else {
                ctx.alloc_vreg(SpillDesc::transient())
            };
            block.push(MInst::Neg {
                dst: fill,
                src: repeated,
            });
            if result_width != 64 {
                ctx.emit_and_imm(block, result, fill, mask_for_width(result_width));
            }
            return result;
        }

        let fill = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Neg {
            dst: fill,
            src: repeated,
        });
        let high = if result_width == 64 {
            let high = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShlImm {
                dst: high,
                src: fill,
                imm: suffix_width as u8,
            });
            high
        } else {
            let high = ctx.alloc_vreg(SpillDesc::transient());
            let high_mask = mask_for_width(result_width) & !mask_for_width(suffix_width);
            ctx.emit_and_imm(block, high, fill, high_mask);
            high
        };
        let result = destination.unwrap_or_else(|| ctx.alloc_vreg(SpillDesc::transient()));
        block.push(MInst::Or {
            dst: result,
            lhs: suffix,
            rhs: high,
        });
        result
    }

    let destination = ctx.reg_map.get(dst);
    let value = lower_plane(
        ctx,
        block,
        ctx.reg_map.get(repeated),
        ctx.reg_map.get(suffix),
        suffix_width,
        result_width,
        Some(destination),
    );
    debug_assert_eq!(value, destination);

    if ctx.four_state {
        let repeated_mask = ctx.get_mask(repeated, block);
        let suffix_mask = ctx.get_mask(suffix, block);
        let result_mask = lower_plane(
            ctx,
            block,
            repeated_mask,
            suffix_mask,
            suffix_width,
            result_width,
            None,
        );
        ctx.set_mask(dst, result_mask);
    }

    true
}

pub(super) fn try_lower_concat_of_muxes(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    args: &[RegisterId],
    sir_block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    sir_defs: &HashMap<RegisterId, usize>,
) -> bool {
    if args.len() < 2
        || (ctx.four_state
            && args
                .iter()
                .any(|arg| !ctx.single_use_registers.contains(arg)))
    {
        return false;
    }

    let total_width = args.iter().map(|arg| ctx.sir_width(arg)).sum::<usize>();
    if total_width == 0 || total_width != ctx.sir_width(&dst) {
        return false;
    }
    if ctx.four_state && args.len() < 2 * ISelContext::num_chunks(total_width) {
        return false;
    }

    let mut cond = None;
    let mut then_parts = Vec::with_capacity(args.len());
    let mut else_parts = Vec::with_capacity(args.len());

    for &arg in args {
        let Some(&idx) = sir_defs.get(&arg) else {
            return false;
        };
        let SIRInstruction::Mux(mux_dst, mux_cond, then_val, else_val) =
            sir_block.instructions[idx]
        else {
            return false;
        };
        if mux_dst != arg {
            return false;
        }
        if let Some(existing_cond) = cond {
            if existing_cond != mux_cond {
                return false;
            }
        } else {
            cond = Some(mux_cond);
        }
        let width = ctx.sir_width(&arg);
        if ctx.sir_width(&then_val) < width || ctx.sir_width(&else_val) < width {
            return false;
        }
        then_parts.push((then_val, width));
        else_parts.push((else_val, width));
    }

    let cond = cond.expect("non-empty mux concat must have a condition");
    let (cond_vreg, cond_unknown) = lower_mux_condition_state(ctx, block, cond);

    let then_chunks = lower_concat_register_parts_to_chunks(
        ctx,
        block,
        &then_parts.iter().copied().rev().collect::<Vec<_>>(),
        total_width,
        false,
        sir_block,
        sir_defs,
    );
    let else_chunks = lower_concat_register_parts_to_chunks(
        ctx,
        block,
        &else_parts.iter().copied().rev().collect::<Vec<_>>(),
        total_width,
        false,
        sir_block,
        sir_defs,
    );
    let result_chunks = if ctx.four_state {
        let then_masks = lower_concat_register_parts_to_chunks(
            ctx,
            block,
            &then_parts.iter().copied().rev().collect::<Vec<_>>(),
            total_width,
            true,
            sir_block,
            sir_defs,
        );
        let else_masks = lower_concat_register_parts_to_chunks(
            ctx,
            block,
            &else_parts.iter().copied().rev().collect::<Vec<_>>(),
            total_width,
            true,
            sir_block,
            sir_defs,
        );
        let mut values = Vec::with_capacity(then_chunks.len());
        let mut masks = Vec::with_capacity(then_chunks.len());
        for (index, &(then_value, width)) in then_chunks.iter().enumerate() {
            let (value, mask) = lower_four_state_mux_chunk(
                ctx,
                block,
                cond_vreg,
                cond_unknown,
                then_value,
                then_masks[index].0,
                else_chunks[index].0,
                else_masks[index].0,
                width,
            );
            values.push((value, width));
            masks.push((mask, width));
        }
        ctx.set_mask(dst, masks[0].0);
        if total_width > 64 {
            ctx.wide_masks.insert(dst, masks);
        }
        values
    } else {
        lower_mux_chunk_blend(
            ctx,
            block,
            cond_vreg,
            &then_chunks,
            &else_chunks,
            total_width,
        )
    };

    if total_width <= 64 {
        let dst_vreg = ctx.reg_map.get(dst);
        if let Some(&(result, _)) = result_chunks.first() {
            ctx.emit_mov(block, dst_vreg, result);
            ctx.known_bits.insert(dst_vreg, total_width);
        }
    } else {
        ctx.set_wide_chunks(dst, result_chunks);
    }
    true
}

/// Recover captured words after a pure Slice/Concat round trip. Unlike
/// arithmetic rewrites this copies both planes and preserves Z unchanged.
pub(super) fn try_lower_concat_of_adjacent_slices(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    destination: RegisterId,
    arguments: &[RegisterId],
    sir_block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, usize>,
) -> bool {
    let slice = |register: RegisterId| {
        let index = *definitions.get(&register)?;
        match sir_block.instructions[index] {
            SIRInstruction::Slice(_, source, offset, width)
                if width == ctx.sir_width(&register) =>
            {
                Some((source, offset, width))
            }
            _ => None,
        }
    };
    if !arguments
        .windows(2)
        .any(|pair| match (slice(pair[1]), slice(pair[0])) {
            (Some((a, offset, width)), Some((b, next, next_width))) => {
                a == b && offset + width == next && next + next_width <= ctx.sir_width(&a)
            }
            _ => false,
        })
    {
        return false;
    }
    let parts: Vec<_> = arguments
        .iter()
        .rev()
        .map(|&register| (register, ctx.sir_width(&register)))
        .collect();
    let width = ctx.sir_width(&destination);
    if width == 0 || parts.iter().map(|part| part.1).sum::<usize>() != width {
        return false;
    }
    let values = lower_concat_register_parts_to_chunks(
        ctx,
        block,
        &parts,
        width,
        false,
        sir_block,
        definitions,
    );
    ctx.set_wide_chunks(destination, values);
    if ctx.four_state {
        let masks = lower_concat_register_parts_to_chunks(
            ctx,
            block,
            &parts,
            width,
            true,
            sir_block,
            definitions,
        );
        ctx.set_mask(destination, masks[0].0);
        if width > 64 {
            ctx.wide_masks.insert(destination, masks);
        }
    }
    true
}

/// Pack low-to-high SSA parts, recovering adjacent slices of a captured
/// source before packing. This avoids splitting a word into lanes only to
/// assemble the same word again. Never reload its physical state home:
/// intervening stores must not change the captured value or unknown plane.
fn lower_concat_register_parts_to_chunks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    parts: &[(RegisterId, usize)],
    total_width: usize,
    mask: bool,
    sir_block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, usize>,
) -> Vec<(VReg, usize)> {
    let slice = |register: RegisterId, width: usize| {
        let index = *definitions.get(&register)?;
        match sir_block.instructions[index] {
            SIRInstruction::Slice(_, source, offset, slice_width) if slice_width == width => {
                Some((source, offset))
            }
            _ => None,
        }
    };
    let mut flat = Vec::with_capacity(parts.len());
    let mut index = 0;
    while index < parts.len() {
        let (register, width) = parts[index];
        let mut end = index + 1;
        let mut combined_width = width;
        let mut recovered = None;
        if let Some((source, offset)) = slice(register, width) {
            while let Some(&(next, next_width)) = parts.get(end) {
                if slice(next, next_width) != Some((source, offset + combined_width)) {
                    break;
                }
                combined_width += next_width;
                end += 1;
            }
            if end > index + 1 && offset + combined_width <= ctx.sir_width(&source) {
                recovered = Some((source, offset));
            }
        }
        let (source, offset, take_width) = if let Some((source, offset)) = recovered {
            (source, offset, combined_width)
        } else {
            end = index + 1;
            (register, 0, width)
        };
        let chunks = if mask {
            get_wide_mask_chunks(
                ctx,
                block,
                &source,
                ISelContext::num_chunks(ctx.sir_width(&source)),
            )
        } else {
            ctx.get_wide_chunks(&source, block)
                .into_iter()
                .map(|chunk| chunk.0)
                .collect()
        };
        let mut consumed = 0;
        while consumed < take_width {
            let position = offset + consumed;
            let shift = position % 64;
            let take = (64 - shift).min(take_width - consumed);
            let mut value = chunks[position / 64];
            if shift > 0 {
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: shifted,
                    src: value,
                    imm: shift as u8,
                });
                value = shifted;
            }
            flat.push((value, take));
            consumed += take;
        }
        index = end;
    }
    lower_flat_concat_to_chunks(ctx, block, flat, total_width)
}

/// Replace an adjacent run of the same canonical one-bit value by one fill
/// word.  The rewrite is in-place: its output never has more parts than its
/// input, including runs longer than one machine word.
fn collapse_repeated_single_bit_concat_parts(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    mut parts: Vec<(VReg, usize)>,
) -> Vec<(VReg, usize)> {
    const MIN_REPEATED_BITS: usize = 4;

    let mut read = 0usize;
    let mut write = 0usize;
    while read < parts.len() {
        let (source, width) = parts[read];
        if width != 1 {
            parts[write] = parts[read];
            read += 1;
            write += 1;
            continue;
        }

        let mut run_end = read + 1;
        while run_end < parts.len() && parts[run_end] == (source, 1) {
            run_end += 1;
        }
        let run_width = run_end - read;
        if run_width < MIN_REPEATED_BITS {
            while read < run_end {
                parts[write] = parts[read];
                read += 1;
                write += 1;
            }
            continue;
        }

        let fill = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Neg {
            dst: fill,
            src: source,
        });
        let mut remaining = run_width;
        while remaining != 0 {
            let take = remaining.min(64);
            parts[write] = (fill, take);
            write += 1;
            remaining -= take;
        }
        read = run_end;
    }
    parts.truncate(write);
    parts
}

/// Repack an LSB-first stream of at-most-machine-word pieces into canonical
/// 64-bit chunks.  Each source part is consumed once, so lowering is linear in
/// the number of concat parts plus produced chunks.
pub(super) fn lower_flat_concat_to_chunks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    flat_bits: Vec<(VReg, usize)>,
    total_width: usize,
) -> Vec<(VReg, usize)> {
    let flat_bits = collapse_repeated_single_bit_concat_parts(ctx, block, flat_bits);
    debug_assert_eq!(
        flat_bits.iter().map(|(_, width)| *width).sum::<usize>(),
        total_width
    );
    debug_assert!(flat_bits.iter().all(|(_, width)| (1..=64).contains(width)));

    let n_dst_chunks = ISelContext::num_chunks(total_width);
    let mut dst_chunks = Vec::with_capacity(n_dst_chunks);
    let mut flat_idx = 0usize;
    let mut flat_consumed = 0usize;

    for chunk_i in 0..n_dst_chunks {
        let chunk_width = if chunk_i == n_dst_chunks - 1 {
            let rem = total_width % 64;
            if rem == 0 { 64 } else { rem }
        } else {
            64
        };

        let mut acc = None;
        let mut acc_pos = 0usize;

        while acc_pos < chunk_width && flat_idx < flat_bits.len() {
            let (fv, fw) = flat_bits[flat_idx];
            let remaining_in_flat = fw - flat_consumed;
            let need = chunk_width - acc_pos;
            let take = remaining_in_flat.min(need);

            let mut piece = fv;
            if flat_consumed > 0 {
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: shifted,
                    src: piece,
                    imm: flat_consumed as u8,
                });
                piece = shifted;
            }
            if take < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, piece, mask_for_width(take));
                piece = masked;
            }

            if acc_pos > 0 {
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShlImm {
                    dst: shifted,
                    src: piece,
                    imm: acc_pos as u8,
                });
                piece = shifted;
            }

            acc = Some(match acc {
                None => piece,
                Some(previous) => {
                    let merged = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or {
                        dst: merged,
                        lhs: previous,
                        rhs: piece,
                    });
                    merged
                }
            });

            acc_pos += take;
            flat_consumed += take;
            if flat_consumed >= fw {
                flat_idx += 1;
                flat_consumed = 0;
            }
        }

        let acc = acc.unwrap_or_else(|| {
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            zero
        });
        dst_chunks.push((acc, chunk_width));
    }

    dst_chunks
}

fn lower_mux_chunk_blend(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    cond_vreg: VReg,
    then_chunks: &[(VReg, usize)],
    else_chunks: &[(VReg, usize)],
    total_width: usize,
) -> Vec<(VReg, usize)> {
    let n_chunks = ISelContext::num_chunks(total_width);
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let cond_bc_raw = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Sub {
        dst: cond_bc_raw,
        lhs: zero,
        rhs: cond_vreg,
    });

    let mut result_chunks = Vec::with_capacity(n_chunks);
    for i in 0..n_chunks {
        let chunk_width = if i == n_chunks - 1 {
            let rem = total_width % 64;
            if rem == 0 { 64 } else { rem }
        } else {
            64
        };
        let tv = then_chunks.get(i).map(|&(v, _)| v).unwrap_or(zero);
        let ev = else_chunks.get(i).map(|&(v, _)| v).unwrap_or(zero);
        let cond_bc = if chunk_width < 64 {
            let masked = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked, cond_bc_raw, mask_for_width(chunk_width));
            masked
        } else {
            cond_bc_raw
        };

        let result = if tv == ev {
            tv
        } else {
            let diff = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Xor {
                dst: diff,
                lhs: tv,
                rhs: ev,
            });
            let selected_diff = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: selected_diff,
                lhs: diff,
                rhs: cond_bc,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Xor {
                dst: res,
                lhs: ev,
                rhs: selected_diff,
            });
            res
        };
        result_chunks.push((result, chunk_width));
    }

    result_chunks
}

/// Lift independently computed bitwise lanes before assembling their result.
/// Every lifted operation performs its original four-state normalization;
/// packing the captured operands themselves preserves raw Z encodings.
pub(super) fn try_lower_concat_of_bitwise_lanes(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    destination: RegisterId,
    arguments: &[RegisterId],
    sir_block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, usize>,
) -> bool {
    let width = ctx.sir_width(&destination);
    if !ctx.four_state || arguments.len() < 3 || width > 64 {
        return false;
    }
    let parts: Vec<_> = arguments
        .iter()
        .rev()
        .map(|&register| (register, ctx.sir_width(&register)))
        .collect();
    let Some(lanes) = match_bitwise_lanes(ctx, &parts, sir_block, definitions) else {
        return false;
    };
    let (value, mask) = lower_bitwise_lanes(ctx, block, lanes, width, sir_block, definitions, 0);
    let dst = ctx.reg_map.get(destination);
    ctx.emit_mov(block, dst, value);
    ctx.set_mask(destination, mask);
    true
}

struct BitwiseLanes {
    operation: BinaryOp,
    left: Vec<(RegisterId, usize)>,
    right: Vec<(RegisterId, usize)>,
}

fn match_bitwise_lanes(
    ctx: &ISelContext,
    parts: &[(RegisterId, usize)],
    sir_block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, usize>,
) -> Option<BitwiseLanes> {
    let mut operation = None;
    let mut left = Vec::with_capacity(parts.len());
    let mut right = Vec::with_capacity(parts.len());
    for &(register, width) in parts {
        if width == 0 || !ctx.single_use_registers.contains(&register) {
            return None;
        }
        let index = *definitions.get(&register)?;
        let SIRInstruction::Binary(_, lhs, op, rhs) = sir_block.instructions[index] else {
            return None;
        };
        let op = match op {
            BinaryOp::LogicAnd if width == 1 => BinaryOp::And,
            BinaryOp::LogicOr if width == 1 => BinaryOp::Or,
            BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => op,
            _ => return None,
        };
        if operation.is_some_and(|old| old != op)
            || ctx.sir_width(&lhs) != width
            || ctx.sir_width(&rhs) != width
        {
            return None;
        }
        operation = Some(op);
        left.push((lhs, width));
        right.push((rhs, width));
    }
    Some(BitwiseLanes {
        operation: operation?,
        left,
        right,
    })
}

fn lower_bitwise_lane_operands(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    parts: &[(RegisterId, usize)],
    width: usize,
    sir_block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, usize>,
    depth: usize,
) -> (VReg, VReg) {
    // Each lifted register has exactly one use. Recursion therefore consumes
    // distinct source definitions rather than expanding a shared expression
    // DAG. Bound its depth as well, keeping compiler stack use independent of
    // an adversarially deep lane expression.
    if depth < 16
        && let Some(lanes) = match_bitwise_lanes(ctx, parts, sir_block, definitions)
    {
        return lower_bitwise_lanes(ctx, block, lanes, width, sir_block, definitions, depth + 1);
    }
    let value = lower_concat_register_parts_to_chunks(
        ctx,
        block,
        parts,
        width,
        false,
        sir_block,
        definitions,
    )[0]
    .0;
    let mask = lower_concat_register_parts_to_chunks(
        ctx,
        block,
        parts,
        width,
        true,
        sir_block,
        definitions,
    )[0]
    .0;
    (value, mask)
}

fn lower_bitwise_lanes(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    lanes: BitwiseLanes,
    width: usize,
    sir_block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, usize>,
    depth: usize,
) -> (VReg, VReg) {
    let (lhs, lm) = lower_bitwise_lane_operands(
        ctx,
        block,
        &lanes.left,
        width,
        sir_block,
        definitions,
        depth,
    );
    let (rhs, rm) = lower_bitwise_lane_operands(
        ctx,
        block,
        &lanes.right,
        width,
        sir_block,
        definitions,
        depth,
    );
    let mask = lower_binary_mask(ctx, block, &lanes.operation, lhs, rhs, lm, rm, width);
    let value = ctx.alloc_vreg(SpillDesc::transient());
    block.push(match lanes.operation {
        BinaryOp::And => MInst::And {
            dst: value,
            lhs,
            rhs,
        },
        BinaryOp::Or => MInst::Or {
            dst: value,
            lhs,
            rhs,
        },
        BinaryOp::Xor => MInst::Xor {
            dst: value,
            lhs,
            rhs,
        },
        _ => unreachable!(),
    });
    let normalized = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: normalized,
        lhs: value,
        rhs: mask,
    });
    (normalized, mask)
}
