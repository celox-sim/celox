//! Bit insertion, extraction, and population-count idiom recognition.

use super::*;

// ────────────────────────────────────────────────────────────────
// Existing passes
// ────────────────────────────────────────────────────────────────

/// Fold a single-bit clear-and-insert toggle into XOR.
///
/// Pattern:
///   `(x & ~(1 << s)) | ((((x >> s) & 1) ^ 1) << s)`
///
/// This is produced by dynamic bit-select XOR assignment such as
/// `x[s] ^= 1`. For 2-state values it is equivalent to `x ^ (1 << s)`.
pub(super) fn fold_bit_toggle_insert(func: &mut MFunction) {
    let mut defs: HashMap<VReg, &MInst> = HashMap::default();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(d) = inst.def() {
                defs.insert(d, inst);
            }
        }
    }

    let mut replacements = Vec::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        for (inst_index, inst) in block.insts.iter().enumerate() {
            let MInst::Or { dst, lhs, rhs } = *inst else {
                continue;
            };

            if let Some((value, mask)) = match_bit_toggle_insert(lhs, rhs, &defs)
                .or_else(|| match_bit_toggle_insert(rhs, lhs, &defs))
            {
                let replacement = MInst::Xor {
                    dst,
                    lhs: value,
                    rhs: mask,
                };
                replacements.push((block_index, inst_index, replacement));
            }
        }
    }
    drop(defs);
    for (block, inst, replacement) in replacements {
        func.blocks[block].insts[inst] = replacement;
    }
}

fn match_bit_toggle_insert(
    clear_part: VReg,
    insert_part: VReg,
    defs: &HashMap<VReg, &MInst>,
) -> Option<(VReg, VReg)> {
    let MInst::And {
        lhs: clear_lhs,
        rhs: clear_rhs,
        ..
    } = defs.get(&clear_part)?
    else {
        return None;
    };

    let (value, inverted_mask) = match defs.get(clear_lhs) {
        Some(MInst::BitNot { .. }) => (*clear_rhs, *clear_lhs),
        _ => match defs.get(clear_rhs) {
            Some(MInst::BitNot { .. }) => (*clear_lhs, *clear_rhs),
            _ => return None,
        },
    };

    let MInst::BitNot { src: mask, .. } = defs.get(&inverted_mask)? else {
        return None;
    };

    let MInst::Shl {
        lhs: one_for_mask,
        rhs: shift_for_mask,
        ..
    } = defs.get(mask)?
    else {
        return None;
    };
    if !is_const_one(*one_for_mask, defs) {
        return None;
    }

    let MInst::Shl {
        lhs: toggled_bit,
        rhs: shift_for_insert,
        ..
    } = defs.get(&insert_part)?
    else {
        return None;
    };
    if shift_for_insert != shift_for_mask {
        return None;
    }

    let MInst::Xor {
        lhs: xor_lhs,
        rhs: xor_rhs,
        ..
    } = defs.get(toggled_bit)?
    else {
        return None;
    };

    let extracted_bit = if is_const_one(*xor_lhs, defs) {
        *xor_rhs
    } else if is_const_one(*xor_rhs, defs) {
        *xor_lhs
    } else {
        return None;
    };

    let MInst::And {
        lhs: bit_lhs,
        rhs: bit_rhs,
        ..
    } = defs.get(&extracted_bit)?
    else {
        return None;
    };
    let shifted_value = if is_const_one(*bit_lhs, defs) {
        *bit_rhs
    } else if is_const_one(*bit_rhs, defs) {
        *bit_lhs
    } else {
        return None;
    };

    let MInst::Shr {
        lhs: shifted_src,
        rhs: shift_for_extract,
        ..
    } = defs.get(&shifted_value)?
    else {
        return None;
    };

    if *shifted_src == value && shift_for_extract == shift_for_mask {
        Some((value, *mask))
    } else {
        None
    }
}

fn is_const_one(reg: VReg, defs: &HashMap<VReg, &MInst>) -> bool {
    matches!(defs.get(&reg), Some(MInst::LoadImm { value: 1, .. }))
}

/// Fold the exact SWAR expansion of an 8-bit byte enable into BMI2 PDEP.
///
/// The SIR byte-lane blend deliberately expresses the expansion with ordinary
/// arithmetic so its semantics remain target-independent:
///
/// ```text
/// x = (x | x << 28) & 0x0000000f0000000f
/// x = (x | x << 14) & 0x0003000300030003
/// x = (x | x <<  7) & 0x0101010101010101
/// ```
///
/// On BMI2 this is exactly `pdep(enable, 0x0101010101010101)`. Matching the
/// complete constant sequence prevents this target fold from becoming a
/// speculative known-bits rewrite.
pub(super) fn fold_byte_enable_spread_to_pdep(func: &mut MFunction) {
    let defs = func
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|instruction| {
            instruction
                .def()
                .map(|definition| (definition, instruction))
        })
        .collect::<HashMap<_, _>>();

    let mut replacements = Vec::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        for (inst_index, instruction) in block.insts.iter().enumerate() {
            let Some(dst) = instruction.def() else {
                continue;
            };
            let Some((enable, lane_mask)) = match_byte_enable_spread(dst, &defs) else {
                continue;
            };
            let replacement = MInst::Pdep {
                dst,
                src: enable,
                mask: lane_mask,
            };
            replacements.push((block_index, inst_index, replacement));
        }
    }
    drop(defs);
    for (block, inst, replacement) in replacements {
        func.blocks[block].insts[inst] = replacement;
    }
}

fn match_byte_enable_spread(result: VReg, defs: &HashMap<VReg, &MInst>) -> Option<(VReg, VReg)> {
    let (spread7, lane_mask) = and_with_constant(result, 0x0101_0101_0101_0101, defs)?;
    let masked14 = or_with_shifted_self(spread7, 7, defs)?;
    let (spread14, _) = and_with_constant(masked14, 0x0003_0003_0003_0003, defs)?;
    let masked28 = or_with_shifted_self(spread14, 14, defs)?;
    let (spread28, _) = and_with_constant(masked28, 0x0000_000f_0000_000f, defs)?;
    let enable = or_with_shifted_self(spread28, 28, defs)?;
    Some((enable, lane_mask))
}

fn and_with_constant(
    result: VReg,
    expected: u64,
    defs: &HashMap<VReg, &MInst>,
) -> Option<(VReg, VReg)> {
    let MInst::And { lhs, rhs, .. } = defs.get(&result)? else {
        return None;
    };
    if matches!(defs.get(rhs), Some(MInst::LoadImm { value, .. }) if *value == expected) {
        Some((*lhs, *rhs))
    } else if matches!(defs.get(lhs), Some(MInst::LoadImm { value, .. }) if *value == expected) {
        Some((*rhs, *lhs))
    } else {
        None
    }
}

fn or_with_shifted_self(
    result: VReg,
    expected_shift: u8,
    defs: &HashMap<VReg, &MInst>,
) -> Option<VReg> {
    let MInst::Or { lhs, rhs, .. } = defs.get(&result)? else {
        return None;
    };
    if shifted_source(*rhs, expected_shift, defs) == Some(*lhs) {
        Some(*lhs)
    } else if shifted_source(*lhs, expected_shift, defs) == Some(*rhs) {
        Some(*rhs)
    } else {
        None
    }
}

fn shifted_source(result: VReg, expected_shift: u8, defs: &HashMap<VReg, &MInst>) -> Option<VReg> {
    match defs.get(&result)? {
        MInst::ShlImm { src, imm, .. } if *imm == expected_shift => Some(*src),
        MInst::Shl { lhs, rhs, .. }
            if matches!(
                defs.get(rhs),
                Some(MInst::LoadImm { value, .. }) if *value == u64::from(expected_shift)
            ) =>
        {
            Some(*lhs)
        }
        _ => None,
    }
}

/// Fold a bit-deposit OR chain into BMI2 PDEP.
///
/// Pattern:
///   `((src[0] << d0) | (src[1] << d1) | ...)`
/// where source bits are the contiguous low bits `0..N` and destination bits
/// are strictly increasing. This is exactly `pdep(src, mask)`.
pub(super) fn fold_deposit_chain_to_pdep(func: &mut MFunction) {
    let mut defs: HashMap<VReg, &MInst> = HashMap::default();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(d) = inst.def() {
                defs.insert(d, inst);
            }
        }
    }

    let mut replacements = Vec::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        for (inst_idx, inst) in block.insts.iter().enumerate() {
            let Some(dst) = inst.def() else { continue };
            if !matches!(inst, MInst::Or { .. } | MInst::OrImm { .. }) {
                continue;
            }

            let mut chunks: Vec<(u8, u8, u8)> = Vec::new();
            let mut source_reg: Option<VReg> = None;
            if !collect_deposit_chain_chunks(dst, &defs, &mut chunks, &mut source_reg) {
                continue;
            }

            let Some(src) = source_reg else { continue };
            let total_width: usize = chunks.iter().map(|(_, width, _)| *width as usize).sum();
            if !(8..=64).contains(&total_width) {
                continue;
            }
            chunks.sort_unstable();

            let mut mask_val = 0u64;
            let mut expected_src_lsb = 0u8;
            let mut prev_dst_end = 0u8;
            let mut valid = true;
            for &(src_lsb, width, dst_lsb) in &chunks {
                if width == 0
                    || src_lsb != expected_src_lsb
                    || src_lsb as u16 + width as u16 > 64
                    || dst_lsb as u16 + width as u16 > 64
                    || dst_lsb < prev_dst_end
                {
                    valid = false;
                    break;
                }
                for bit in dst_lsb..dst_lsb + width {
                    mask_val |= 1u64 << bit;
                }
                expected_src_lsb += width;
                prev_dst_end = dst_lsb + width;
            }
            if !valid || mask_val == 0 {
                continue;
            }

            let new_insts = if mask_width(mask_val) == Some(total_width) {
                if mask_val == u64::MAX {
                    vec![MInst::Mov { dst, src }]
                } else if u32::try_from(mask_val).is_ok() {
                    vec![MInst::AndImm {
                        dst,
                        src,
                        imm: mask_val,
                    }]
                } else {
                    let mask_vreg = func.vregs.alloc();
                    while func.spill_descs.len() <= mask_vreg.0 as usize {
                        func.spill_descs.push(SpillDesc::remat(mask_val));
                    }
                    vec![
                        MInst::LoadImm {
                            dst: mask_vreg,
                            value: mask_val,
                        },
                        MInst::And {
                            dst,
                            lhs: src,
                            rhs: mask_vreg,
                        },
                    ]
                }
            } else {
                let mask_vreg = func.vregs.alloc();
                while func.spill_descs.len() <= mask_vreg.0 as usize {
                    func.spill_descs.push(SpillDesc::remat(mask_val));
                }
                vec![
                    MInst::LoadImm {
                        dst: mask_vreg,
                        value: mask_val,
                    },
                    MInst::Pdep {
                        dst,
                        src,
                        mask: mask_vreg,
                    },
                ]
            };

            replacements.push((block_index, inst_idx, new_insts));
        }
    }
    drop(defs);
    // Reverse instruction order preserves indices within each block.
    for (block, idx, new_insts) in replacements.into_iter().rev() {
        func.blocks[block].insts.splice(idx..=idx, new_insts);
    }
}

fn collect_deposit_chain_chunks(
    reg: VReg,
    defs: &HashMap<VReg, &MInst>,
    chunks: &mut Vec<(u8, u8, u8)>,
    source_reg: &mut Option<VReg>,
) -> bool {
    let Some(def) = defs.get(&reg) else {
        return false;
    };

    match def {
        MInst::Or { lhs, rhs, .. } => {
            collect_deposit_chain_chunks(*lhs, defs, chunks, source_reg)
                && collect_deposit_chain_chunks(*rhs, defs, chunks, source_reg)
        }
        MInst::OrImm { src, imm, .. } if *imm == 0 => {
            collect_deposit_chain_chunks(*src, defs, chunks, source_reg)
        }
        MInst::Mov { src, .. } => collect_deposit_chain_chunks(*src, defs, chunks, source_reg),
        _ => collect_deposit_term(reg, defs, chunks, source_reg),
    }
}

fn collect_deposit_term(
    reg: VReg,
    defs: &HashMap<VReg, &MInst>,
    chunks: &mut Vec<(u8, u8, u8)>,
    source_reg: &mut Option<VReg>,
) -> bool {
    let Some((src, src_lsb, width, dst_lsb)) = trace_deposit_term(reg, defs) else {
        return false;
    };
    match source_reg {
        Some(existing) if *existing != src => return false,
        None => *source_reg = Some(src),
        _ => {}
    }
    chunks.push((src_lsb, width, dst_lsb));
    true
}

fn trace_deposit_term(reg: VReg, defs: &HashMap<VReg, &MInst>) -> Option<(VReg, u8, u8, u8)> {
    trace_deposit_term_inner(reg, defs)
        .filter(|(_, _, width, dst_lsb)| *width > 0 && (*dst_lsb as u16 + *width as u16) <= 64)
}

fn trace_deposit_term_inner(reg: VReg, defs: &HashMap<VReg, &MInst>) -> Option<(VReg, u8, u8, u8)> {
    let Some(def) = defs.get(&reg) else {
        return Some((reg, 0, 64, 0));
    };
    match def {
        MInst::Mov { src, .. } => trace_deposit_term_inner(*src, defs),
        MInst::ShlImm { src, imm, .. } if *imm < 64 => {
            let (base, src_lsb, width) = trace_value_window(*src, defs)?;
            Some((base, src_lsb, width.min(64 - *imm), *imm))
        }
        MInst::AndImm { src, imm, .. } => {
            let (base, src_lsb, width, dst_lsb) = trace_deposit_term_inner(*src, defs)?;
            let mask_w = mask_width(*imm)? as u8;
            Some((
                base,
                src_lsb,
                width.min(mask_w.saturating_sub(dst_lsb)),
                dst_lsb,
            ))
        }
        MInst::And { lhs, rhs, .. } => {
            if let Some(mask) = load_imm_value(*lhs, defs) {
                let (base, src_lsb, width, dst_lsb) = trace_deposit_term_inner(*rhs, defs)?;
                let mask_w = mask_width(mask)? as u8;
                Some((
                    base,
                    src_lsb,
                    width.min(mask_w.saturating_sub(dst_lsb)),
                    dst_lsb,
                ))
            } else if let Some(mask) = load_imm_value(*rhs, defs) {
                let (base, src_lsb, width, dst_lsb) = trace_deposit_term_inner(*lhs, defs)?;
                let mask_w = mask_width(mask)? as u8;
                Some((
                    base,
                    src_lsb,
                    width.min(mask_w.saturating_sub(dst_lsb)),
                    dst_lsb,
                ))
            } else {
                None
            }
        }
        _ => {
            let (base, src_lsb, width) = trace_value_window(reg, defs)?;
            Some((base, src_lsb, width, 0))
        }
    }
}

fn trace_value_window(reg: VReg, defs: &HashMap<VReg, &MInst>) -> Option<(VReg, u8, u8)> {
    let Some(def) = defs.get(&reg) else {
        return Some((reg, 0, 64));
    };
    match def {
        MInst::Mov { src, .. } => trace_value_window(*src, defs),
        MInst::ShrImm { src, imm, .. } => {
            let (base, lsb, width) = trace_value_window(*src, defs).unwrap_or((*src, 0, 64));
            let new_lsb = lsb.checked_add(*imm)?;
            Some((base, new_lsb, width.saturating_sub(*imm)))
        }
        MInst::AndImm { src, imm, .. } => {
            let mask_w = mask_width(*imm)? as u8;
            if let Some((base, lsb, width)) = trace_value_window(*src, defs) {
                Some((base, lsb, width.min(mask_w)))
            } else {
                Some((reg, 0, mask_w))
            }
        }
        MInst::And { lhs, rhs, .. } => {
            if let Some(mask) = load_imm_value(*lhs, defs) {
                let mask_w = mask_width(mask)? as u8;
                if let Some((base, lsb, width)) = trace_value_window(*rhs, defs) {
                    Some((base, lsb, width.min(mask_w)))
                } else {
                    Some((reg, 0, mask_w))
                }
            } else if let Some(mask) = load_imm_value(*rhs, defs) {
                let mask_w = mask_width(mask)? as u8;
                if let Some((base, lsb, width)) = trace_value_window(*lhs, defs) {
                    Some((base, lsb, width.min(mask_w)))
                } else {
                    Some((reg, 0, mask_w))
                }
            } else {
                None
            }
        }
        MInst::LoadConstantTableAddr { .. }
        | MInst::Load { .. }
        | MInst::LoadIndexed { .. }
        | MInst::LoadPtr { .. } => Some((reg, 0, 64)),
        _ => None,
    }
}

fn load_imm_value(reg: VReg, defs: &HashMap<VReg, &MInst>) -> Option<u64> {
    match defs.get(&reg)? {
        MInst::LoadImm { value, .. } => Some(*value),
        MInst::Mov { src, .. } => load_imm_value(*src, defs),
        _ => None,
    }
}

/// Fold a bit-extract OR chain into BMI2 PEXT.
///
/// Pattern:
///   `((src >> s0) & lowmask(w0)) << 0
///    | ((src >> s1) & lowmask(w1)) << w0 | ...`
/// where destination chunks are contiguous low bits and source chunks are
/// strictly increasing. This is `pext(src, mask)`.
pub(super) fn fold_extract_chain_to_pext(func: &mut MFunction) {
    let mut defs: HashMap<VReg, &MInst> = HashMap::default();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(d) = inst.def() {
                defs.insert(d, inst);
            }
        }
    }

    let mut replacements = Vec::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        for (inst_idx, inst) in block.insts.iter().enumerate() {
            let Some(dst) = inst.def() else { continue };
            if !matches!(inst, MInst::Or { .. } | MInst::OrImm { .. }) {
                continue;
            }

            let mut chunks: Vec<(u8, u8, u8)> = Vec::new();
            let mut source_reg: Option<VReg> = None;
            if !collect_deposit_chain_chunks(dst, &defs, &mut chunks, &mut source_reg) {
                continue;
            }

            let Some(src) = source_reg else { continue };
            let total_width: usize = chunks.iter().map(|(_, width, _)| *width as usize).sum();
            if !(8..=64).contains(&total_width) {
                continue;
            }
            chunks.sort_unstable_by_key(|(src_lsb, _, _)| *src_lsb);

            let mut mask_val = 0u64;
            let mut expected_dst_lsb = 0u8;
            let mut prev_src_end = 0u8;
            let mut valid = true;
            for &(src_lsb, width, dst_lsb) in &chunks {
                if width == 0
                    || dst_lsb != expected_dst_lsb
                    || src_lsb as u16 + width as u16 > 64
                    || dst_lsb as u16 + width as u16 > 64
                    || src_lsb < prev_src_end
                {
                    valid = false;
                    break;
                }
                for bit in src_lsb..src_lsb + width {
                    mask_val |= 1u64 << bit;
                }
                expected_dst_lsb += width;
                prev_src_end = src_lsb + width;
            }
            if !valid || mask_val == 0 {
                continue;
            }

            let new_insts = if mask_width(mask_val) == Some(total_width) {
                if mask_val == u64::MAX {
                    vec![MInst::Mov { dst, src }]
                } else if u32::try_from(mask_val).is_ok() {
                    vec![MInst::AndImm {
                        dst,
                        src,
                        imm: mask_val,
                    }]
                } else {
                    let mask_vreg = func.vregs.alloc();
                    while func.spill_descs.len() <= mask_vreg.0 as usize {
                        func.spill_descs.push(SpillDesc::remat(mask_val));
                    }
                    vec![
                        MInst::LoadImm {
                            dst: mask_vreg,
                            value: mask_val,
                        },
                        MInst::And {
                            dst,
                            lhs: src,
                            rhs: mask_vreg,
                        },
                    ]
                }
            } else {
                let mask_vreg = func.vregs.alloc();
                while func.spill_descs.len() <= mask_vreg.0 as usize {
                    func.spill_descs.push(SpillDesc::remat(mask_val));
                }
                vec![
                    MInst::LoadImm {
                        dst: mask_vreg,
                        value: mask_val,
                    },
                    MInst::Pext {
                        dst,
                        src,
                        mask: mask_vreg,
                    },
                ]
            };

            replacements.push((block_index, inst_idx, new_insts));
        }
    }
    drop(defs);
    // Reverse instruction order preserves indices within each block.
    for (block, idx, new_insts) in replacements.into_iter().rev() {
        func.blocks[block].insts.splice(idx..=idx, new_insts);
    }
}

/// Fold XOR chains of single-bit extractions from the same source into
/// PEXT + POPCNT + AND 1.
///
/// Pattern: `(src >> a) & 1 ^ (src >> b) & 1 ^ ...` where all extractions
/// come from the same source register.
///
/// Replacement: `pext(src, mask) → popcnt → and 1` where
/// `mask = (1 << a) | (1 << b) | ...`
pub(super) fn fold_xor_chain_to_pext(func: &mut MFunction) {
    // Keep the original definitions borrowed until all rewrites are planned.
    let mut defs: HashMap<VReg, &MInst> = HashMap::default();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(d) = inst.def() {
                defs.insert(d, inst);
            }
        }
    }

    // For each block, scan for Xor instructions and try to fold
    let mut replacements = Vec::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        for (inst_idx, inst) in block.insts.iter().enumerate() {
            // Look for: v = xor a, b  where result is 1-bit (used with and 1)
            let MInst::Xor { dst, lhs, rhs } = inst else {
                continue;
            };

            // Try to collect the full XOR chain and extract bit positions
            let mut bits: Vec<(VReg, u64)> = Vec::new();
            let mut source_reg: Option<VReg> = None;

            let ok = collect_xor_chain_bits(*dst, *lhs, *rhs, &defs, &mut bits, &mut source_reg);
            if !ok {
                continue;
            }

            // Need at least 3 bits to be worth the PEXT overhead
            let Some(src) = source_reg else { continue };
            if bits.len() < 3 {
                continue;
            }

            // Build mask from bit positions
            let mut mask_val: u64 = 0;
            for &(_, pos) in &bits {
                if pos >= 64 {
                    continue;
                } // skip wide
                mask_val |= 1u64 << pos;
            }
            if mask_val == 0 {
                continue;
            }

            // Generate: mask_vreg = imm mask_val
            //           pext_vreg = pext src, mask_vreg
            //           popcnt_vreg = popcnt pext_vreg
            //           dst = and popcnt_vreg, 1
            let mask_vreg = func.vregs.alloc();
            while func.spill_descs.len() <= mask_vreg.0 as usize {
                func.spill_descs.push(SpillDesc::remat(mask_val));
            }
            let pext_vreg = func.vregs.alloc();
            while func.spill_descs.len() <= pext_vreg.0 as usize {
                func.spill_descs.push(SpillDesc::transient());
            }
            let popcnt_vreg = func.vregs.alloc();
            while func.spill_descs.len() <= popcnt_vreg.0 as usize {
                func.spill_descs.push(SpillDesc::transient());
            }

            let new_insts = vec![
                MInst::LoadImm {
                    dst: mask_vreg,
                    value: mask_val,
                },
                MInst::Pext {
                    dst: pext_vreg,
                    src,
                    mask: mask_vreg,
                },
                MInst::Popcnt {
                    dst: popcnt_vreg,
                    src: pext_vreg,
                },
                MInst::AndImm {
                    dst: *dst,
                    src: popcnt_vreg,
                    imm: 1,
                },
            ];
            replacements.push((block_index, inst_idx, new_insts));
        }
    }
    drop(defs);
    // Reverse instruction order preserves indices within each block.
    for (block, idx, new_insts) in replacements.into_iter().rev() {
        func.blocks[block].insts.splice(idx..=idx, new_insts);
    }
}

/// Fold add trees of single-bit extractions from the same source into
/// `and mask` + `popcnt`.
///
/// Pattern: `(src >> a) & 1 + (src >> b) & 1 + ...`
/// Replacement:
///   if mask == all_ones: `popcnt src`
///   else: `masked = and src, mask; popcnt masked`
pub(super) fn fold_add_chain_to_popcnt(func: &mut MFunction) {
    let mut defs: HashMap<VReg, &MInst> = HashMap::default();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(d) = inst.def() {
                defs.insert(d, inst);
            }
        }
    }

    let mut replacements = Vec::new();
    for (block_index, block) in func.blocks.iter().enumerate() {
        for (inst_idx, inst) in block.insts.iter().enumerate() {
            let MInst::Add { dst, lhs, rhs } = inst else {
                continue;
            };

            let mut bits: Vec<(VReg, u64)> = Vec::new();
            let mut source_reg: Option<VReg> = None;

            if !collect_add_chain_bits(*lhs, &defs, &mut bits, &mut source_reg)
                || !collect_add_chain_bits(*rhs, &defs, &mut bits, &mut source_reg)
            {
                continue;
            }

            let Some(src) = source_reg else { continue };
            if bits.len() < 3 {
                continue;
            }

            let mut mask: u64 = 0;
            for &(_, bit) in &bits {
                if bit < 64 {
                    if (mask >> bit) & 1 == 1 {
                        mask = 0;
                        break;
                    }
                    mask |= 1u64 << bit;
                }
            }
            if mask == 0 {
                continue;
            }

            let all_bits_mask = if bits.len() >= 64 {
                u64::MAX
            } else {
                (1u64 << bits.len()) - 1
            };

            let new_insts = if mask == u64::MAX || mask == all_bits_mask {
                vec![MInst::Popcnt { dst: *dst, src }]
            } else {
                let masked_vreg = func.vregs.alloc();
                while func.spill_descs.len() <= masked_vreg.0 as usize {
                    func.spill_descs.push(SpillDesc::transient());
                }
                vec![
                    MInst::AndImm {
                        dst: masked_vreg,
                        src,
                        imm: mask,
                    },
                    MInst::Popcnt {
                        dst: *dst,
                        src: masked_vreg,
                    },
                ]
            };

            replacements.push((block_index, inst_idx, new_insts));
        }
    }
    drop(defs);
    // Reverse instruction order preserves indices within each block.
    for (block, idx, new_insts) in replacements.into_iter().rev() {
        func.blocks[block].insts.splice(idx..=idx, new_insts);
    }
}

/// Recursively collect single-bit extractions from a XOR chain.
/// Returns true if the entire chain consists of single-bit extractions
/// from the same source register.
fn collect_xor_chain_bits(
    _vreg: VReg,
    lhs: VReg,
    rhs: VReg,
    defs: &HashMap<VReg, &MInst>,
    bits: &mut Vec<(VReg, u64)>,
    source_reg: &mut Option<VReg>,
) -> bool {
    // Try to extract a bit from each operand
    for &operand in &[lhs, rhs] {
        if let Some(def_inst) = defs.get(&operand) {
            match def_inst {
                // Pattern: v = xor a, b (recursive)
                MInst::Xor {
                    lhs: l2, rhs: r2, ..
                } => {
                    if !collect_xor_chain_bits(operand, *l2, *r2, defs, bits, source_reg) {
                        return false;
                    }
                }
                // Pattern: v = shr src, imm (bit extraction)
                MInst::ShrImm { src, imm, .. } => {
                    match source_reg {
                        Some(s) if *s != *src => return false, // different source
                        None => *source_reg = Some(*src),
                        _ => {}
                    }
                    bits.push((*src, *imm as u64));
                }
                // Pattern: v = and src, 1 (masked bit — look through)
                MInst::AndImm {
                    src: and_src,
                    imm: 1,
                    ..
                } => {
                    if let Some(inner) = defs.get(and_src) {
                        match inner {
                            MInst::ShrImm { src, imm, .. } => {
                                match source_reg {
                                    Some(s) if *s != *src => return false,
                                    None => *source_reg = Some(*src),
                                    _ => {}
                                }
                                bits.push((*src, *imm as u64));
                            }
                            MInst::Xor {
                                lhs: l2, rhs: r2, ..
                            } => {
                                if !collect_xor_chain_bits(
                                    *and_src, *l2, *r2, defs, bits, source_reg,
                                ) {
                                    return false;
                                }
                            }
                            _ => return false,
                        }
                    } else {
                        return false;
                    }
                }
                _ => return false,
            }
        } else {
            return false;
        }
    }
    true
}

/// Recursively collect single-bit extractions from an add tree.
/// Returns true if the tree contains only 0/1 bit extractions from one source.
fn collect_add_chain_bits(
    reg: VReg,
    defs: &HashMap<VReg, &MInst>,
    bits: &mut Vec<(VReg, u64)>,
    source_reg: &mut Option<VReg>,
) -> bool {
    let Some(def) = defs.get(&reg) else {
        return false;
    };

    match def {
        MInst::Add { lhs, rhs, .. } => {
            collect_add_chain_bits(*lhs, defs, bits, source_reg)
                && collect_add_chain_bits(*rhs, defs, bits, source_reg)
        }
        MInst::Mov { src, .. } => collect_add_chain_bits(*src, defs, bits, source_reg),
        MInst::AddImm { src, imm, .. } if *imm == 0 => {
            collect_add_chain_bits(*src, defs, bits, source_reg)
        }
        MInst::AndImm { src, imm, .. } if *imm == 1 => {
            let Some(inner) = defs.get(src) else {
                return false;
            };
            match inner {
                MInst::ShrImm { src, imm, .. } => {
                    match source_reg {
                        Some(s) if *s != *src => return false,
                        None => *source_reg = Some(*src),
                        _ => {}
                    }
                    bits.push((*src, *imm as u64));
                    true
                }
                MInst::Mov { src, .. } => {
                    match source_reg {
                        Some(s) if *s != *src => return false,
                        None => *source_reg = Some(*src),
                        _ => {}
                    }
                    bits.push((*src, 0));
                    true
                }
                _ => false,
            }
        }
        _ => false,
    }
}
