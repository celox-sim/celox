//! Vectorize Concat: replace a Concat of single-bit extracts from the same
//! source with a single bitwise `And(src, mask)` when each extracted bit
//! occupies the same position in the Concat output as in the source.
//!
//! Handles two patterns:
//!
//! 1. Register-based: `(reg >> K) & 1` or `Slice(reg, K, 1)`
//!    → `And(reg, mask_constant)`
//!
//! 2. Load-based: `Load(addr, K, 1)` from same address
//!    → `Load(addr, 0, width)` then `And(wide_load, mask_constant)`
//!
//! This eliminates O(N) shift+and+or chains for building masked bitvectors
//! such as Hamming parity masks.

use super::pass_manager::ExecutionUnitPass;
use super::shared::{collect_all_used_registers, def_reg, sir_value_to_u64};
use crate::HashMap;
use crate::ir::*;
use crate::optimizer::PassOptions;
use num_bigint::BigUint;

pub(super) struct VectorizeConcatPass;

impl ExecutionUnitPass for VectorizeConcatPass {
    fn name(&self) -> &'static str {
        "vectorize_concat"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        let mut max_reg = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);
        let mut any_changed = false;

        // Build global def map across all blocks
        let mut global_defs: HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>> =
            HashMap::default();
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                if let Some(d) = def_reg(inst) {
                    global_defs.insert(d, inst.clone());
                }
            }
        }

        for block in eu.blocks.values_mut() {
            if vectorize_concats(
                &mut block.instructions,
                &mut eu.register_map,
                &mut max_reg,
                &global_defs,
            ) {
                any_changed = true;
            }
        }

        if !any_changed {
            return;
        }

        let used = collect_all_used_registers(eu);
        for block in eu.blocks.values_mut() {
            block.instructions.retain(|inst| {
                if let Some(d) = def_reg(inst) {
                    used.contains(&d)
                        || matches!(inst, SIRInstruction::Store(..) | SIRInstruction::Commit(..))
                } else {
                    true
                }
            });
        }
    }
}

/// A single-bit element in a Concat: either from a register or from a Load.
enum BitSource {
    /// `(reg >> bit_position) & 1` or `Slice(reg, bit_position, 1)`
    Register {
        source: RegisterId,
        bit_position: usize,
    },
    /// `Load(addr, bit_position, 1)`
    Load {
        addr: RegionedAbsoluteAddr,
        bit_position: usize,
    },
}

/// Try to resolve a register to a single-bit extraction.
fn resolve_bit_source(
    reg: RegisterId,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
) -> Option<BitSource> {
    let def = defs.get(&reg)?;

    match def {
        // Load(dst, addr, Static(offset), 1)
        SIRInstruction::Load(_, addr, SIROffset::Static(offset), 1) => Some(BitSource::Load {
            addr: *addr,
            bit_position: *offset,
        }),

        // Slice(dst, src, offset, 1)
        SIRInstruction::Slice(_, src, offset, 1) => Some(BitSource::Register {
            source: *src,
            bit_position: *offset,
        }),

        // Binary(dst, shifted, And, mask_reg) where mask=1
        SIRInstruction::Binary(_, shifted, BinaryOp::And, mask_reg) => {
            let mask_def = defs.get(mask_reg)?;
            let SIRInstruction::Imm(_, mask_val) = mask_def else {
                return None;
            };
            if sir_value_to_u64(mask_val)? != 1 {
                return None;
            }
            let shifted_def = defs.get(shifted)?;
            match shifted_def {
                SIRInstruction::Binary(_, src, BinaryOp::Shr, shift_reg) => {
                    let shift_def = defs.get(shift_reg)?;
                    let SIRInstruction::Imm(_, shift_val) = shift_def else {
                        return None;
                    };
                    let shift = sir_value_to_u64(shift_val)? as usize;
                    Some(BitSource::Register {
                        source: *src,
                        bit_position: shift,
                    })
                }
                _ => Some(BitSource::Register {
                    source: *shifted,
                    bit_position: 0,
                }),
            }
        }

        // Look through identity
        SIRInstruction::Unary(_, UnaryOp::Ident, src) => resolve_bit_source(*src, defs),

        _ => None,
    }
}

/// Check if a register is a constant zero.
fn is_zero(
    reg: RegisterId,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
) -> bool {
    let Some(def) = defs.get(&reg) else {
        return false;
    };
    matches!(def, SIRInstruction::Imm(_, val) if sir_value_to_u64(val) == Some(0))
}

/// A replacement to apply.
enum Replacement {
    /// Replace Concat with `And(source_reg, mask)`
    RegisterAnd {
        inst_idx: usize,
        dst: RegisterId,
        source: RegisterId,
        mask: u64,
        width: usize,
    },
    /// Replace Concat with `Load(addr, 0, width)` then `And(load, mask)`
    LoadAnd {
        inst_idx: usize,
        dst: RegisterId,
        addr: RegionedAbsoluteAddr,
        mask: u64,
        width: usize,
    },
    /// Replace Concat with grouped shift+mask+or operations.
    /// Used when bits are not in-place but form contiguous groups with constant delta.
    GroupedShift {
        inst_idx: usize,
        dst: RegisterId,
        source: RegisterId,
        /// (src_start, dest_start, group_len)
        groups: Vec<(usize, usize, usize)>,
        width: usize,
    },
}

/// Find contiguous shift groups in a non-in-place Concat.
/// Returns groups as (src_start, dest_start, length).
fn find_shift_groups(
    args: &[RegisterId],
    concat_width: usize,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
) -> Option<Vec<(usize, usize, usize)>> {
    // Collect (src_bit, dest_bit) for each non-zero element
    let mut mappings: Vec<(usize, usize)> = Vec::new();
    for (i, &arg) in args.iter().enumerate() {
        let dest_pos = concat_width - 1 - i;
        if is_zero(arg, defs) {
            continue;
        }
        let info = resolve_bit_source(arg, defs)?;
        let src_pos = match info {
            BitSource::Register { bit_position, .. } => bit_position,
            BitSource::Load { bit_position, .. } => bit_position,
        };
        mappings.push((src_pos, dest_pos));
    }

    if mappings.len() < 3 {
        return None;
    }

    // Sort by src_bit
    mappings.sort_by_key(|&(src, _)| src);

    // Find contiguous groups: consecutive src bits with constant (dest - src) delta
    let mut groups: Vec<(usize, usize, usize)> = Vec::new();
    let mut i = 0;
    while i < mappings.len() {
        let (src_start, dest_start) = mappings[i];
        let delta = dest_start as isize - src_start as isize;
        let mut len = 1usize;

        while i + len < mappings.len() {
            let (next_src, next_dest) = mappings[i + len];
            let next_delta = next_dest as isize - next_src as isize;
            if next_src == src_start + len && next_delta == delta {
                len += 1;
            } else {
                break;
            }
        }

        groups.push((src_start, dest_start, len));
        i += len;
    }

    // Only worth it if we have fewer groups than individual bits
    if groups.len() >= mappings.len() / 2 {
        return None;
    }

    Some(groups)
}

fn vectorize_concats(
    instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    next_reg: &mut usize,
    global_defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
) -> bool {
    let defs = global_defs;

    let mut replacements: Vec<Replacement> = Vec::new();

    for (idx, inst) in instructions.iter().enumerate() {
        let SIRInstruction::Concat(dst, args) = inst else {
            continue;
        };

        let concat_width = args.len();
        if !(3..=64).contains(&concat_width) {
            continue;
        }

        // Check each arg is 1-bit wide
        let all_single_bit = args
            .iter()
            .all(|arg| register_map.get(arg).is_some_and(|rt| rt.width() == 1));
        if !all_single_bit {
            continue;
        }

        // Classify: all from same register source, or all from same Load address
        let mut reg_source: Option<RegisterId> = None;
        let mut load_addr: Option<RegionedAbsoluteAddr> = None;
        let mut mask: u64 = 0;
        let mut in_place = true;
        let mut extract_count = 0usize;
        let mut valid = true;
        let mut is_load_based = false;

        for (i, &arg) in args.iter().enumerate() {
            let concat_position = concat_width - 1 - i; // LSB = 0

            if is_zero(arg, defs) {
                continue;
            }

            match resolve_bit_source(arg, defs) {
                Some(BitSource::Register {
                    source,
                    bit_position,
                }) => {
                    if is_load_based {
                        valid = false;
                        break;
                    }
                    match reg_source {
                        Some(s) if s != source => {
                            valid = false;
                            break;
                        }
                        None => reg_source = Some(source),
                        _ => {}
                    }
                    if bit_position >= 64 {
                        valid = false;
                        break;
                    }
                    if bit_position != concat_position {
                        in_place = false;
                    }
                    mask |= 1u64 << bit_position;
                    extract_count += 1;
                }
                Some(BitSource::Load { addr, bit_position }) => {
                    if reg_source.is_some() {
                        valid = false;
                        break;
                    }
                    is_load_based = true;
                    match load_addr {
                        Some(a) if a != addr => {
                            valid = false;
                            break;
                        }
                        None => load_addr = Some(addr),
                        _ => {}
                    }
                    if bit_position >= 64 {
                        valid = false;
                        break;
                    }
                    if bit_position != concat_position {
                        in_place = false;
                    }
                    mask |= 1u64 << bit_position;
                    extract_count += 1;
                }
                None => {
                    valid = false;
                    break;
                }
            }
        }

        if !valid || extract_count < 3 {
            continue;
        }

        if in_place {
            if let Some(source) = reg_source {
                replacements.push(Replacement::RegisterAnd {
                    inst_idx: idx,
                    dst: *dst,
                    source,
                    mask,
                    width: concat_width,
                });
            } else if let Some(addr) = load_addr {
                replacements.push(Replacement::LoadAnd {
                    inst_idx: idx,
                    dst: *dst,
                    addr,
                    mask,
                    width: concat_width,
                });
            }
        } else if let Some(source) = reg_source {
            // Non-in-place register case: try grouped shift optimization.
            if let Some(groups) = find_shift_groups(args, concat_width, defs) {
                replacements.push(Replacement::GroupedShift {
                    inst_idx: idx,
                    dst: *dst,
                    source,
                    groups,
                    width: concat_width,
                });
            }
        }
    }

    if replacements.is_empty() {
        return false;
    }

    let alloc_reg = |next_reg: &mut usize,
                     register_map: &mut HashMap<RegisterId, RegisterType>,
                     width: usize| {
        *next_reg += 1;
        let reg = RegisterId(*next_reg);
        register_map.insert(
            reg,
            RegisterType::Bit {
                width,
                signed: false,
            },
        );
        reg
    };

    // Apply in reverse to preserve indices
    for repl in replacements.into_iter().rev() {
        // Check if mask covers all bits → And can be omitted
        let is_full_mask = |mask: u64, width: usize| -> bool {
            width <= 64
                && mask
                    == (if width == 64 {
                        u64::MAX
                    } else {
                        (1u64 << width) - 1
                    })
        };

        match repl {
            Replacement::RegisterAnd {
                inst_idx,
                dst,
                source,
                mask,
                width,
            } => {
                if is_full_mask(mask, width) {
                    // All bits extracted → just alias the source
                    instructions[inst_idx] = SIRInstruction::Unary(dst, UnaryOp::Ident, source);
                } else {
                    let mask_reg = alloc_reg(next_reg, register_map, width);
                    let mask_value = SIRValue {
                        payload: BigUint::from(mask),
                        mask: BigUint::ZERO,
                    };
                    instructions.insert(inst_idx, SIRInstruction::Imm(mask_reg, mask_value));
                    instructions[inst_idx + 1] =
                        SIRInstruction::Binary(dst, source, BinaryOp::And, mask_reg);
                }
            }
            Replacement::LoadAnd {
                inst_idx,
                dst,
                addr,
                mask,
                width,
            } => {
                if is_full_mask(mask, width) {
                    // All bits extracted → just a wide Load
                    instructions[inst_idx] =
                        SIRInstruction::Load(dst, addr, SIROffset::Static(0), width);
                } else {
                    let load_reg = alloc_reg(next_reg, register_map, width);
                    let mask_reg = alloc_reg(next_reg, register_map, width);
                    let mask_value = SIRValue {
                        payload: BigUint::from(mask),
                        mask: BigUint::ZERO,
                    };
                    instructions.insert(
                        inst_idx,
                        SIRInstruction::Load(load_reg, addr, SIROffset::Static(0), width),
                    );
                    instructions.insert(inst_idx + 1, SIRInstruction::Imm(mask_reg, mask_value));
                    instructions[inst_idx + 2] =
                        SIRInstruction::Binary(dst, load_reg, BinaryOp::And, mask_reg);
                }
            }
            Replacement::GroupedShift {
                inst_idx,
                dst,
                source,
                groups,
                width,
            } => {
                // Generate: for each group, extract+shift, then OR all together.
                // result = (((src >> s0) & m0) << d0) | (((src >> s1) & m1) << d1) | ...
                let mut new_insts: Vec<SIRInstruction<RegionedAbsoluteAddr>> = Vec::new();
                let mut group_regs: Vec<RegisterId> = Vec::new();

                for &(src_start, dest_start, group_len) in &groups {
                    let group_mask = if group_len >= 64 {
                        u64::MAX
                    } else {
                        (1u64 << group_len) - 1
                    };

                    // Extract: (src >> src_start) & group_mask
                    let extracted = if src_start == 0 {
                        source
                    } else {
                        let shift_reg = alloc_reg(next_reg, register_map, width);
                        let shifted_reg = alloc_reg(next_reg, register_map, width);
                        new_insts.push(SIRInstruction::Imm(
                            shift_reg,
                            SIRValue::new(src_start as u64),
                        ));
                        new_insts.push(SIRInstruction::Binary(
                            shifted_reg,
                            source,
                            BinaryOp::Shr,
                            shift_reg,
                        ));
                        shifted_reg
                    };

                    let masked = if group_mask == u64::MAX || (src_start == 0 && group_len >= width)
                    {
                        extracted
                    } else {
                        let mask_reg = alloc_reg(next_reg, register_map, width);
                        let masked_reg = alloc_reg(next_reg, register_map, width);
                        new_insts.push(SIRInstruction::Imm(mask_reg, SIRValue::new(group_mask)));
                        new_insts.push(SIRInstruction::Binary(
                            masked_reg,
                            extracted,
                            BinaryOp::And,
                            mask_reg,
                        ));
                        masked_reg
                    };

                    // Place: extracted value (bit-0 based) shifted to dest_start
                    let placed = if dest_start == 0 {
                        masked
                    } else {
                        let shift_reg = alloc_reg(next_reg, register_map, width);
                        let placed_reg = alloc_reg(next_reg, register_map, width);
                        new_insts.push(SIRInstruction::Imm(
                            shift_reg,
                            SIRValue::new(dest_start as u64),
                        ));
                        new_insts.push(SIRInstruction::Binary(
                            placed_reg,
                            masked,
                            BinaryOp::Shl,
                            shift_reg,
                        ));
                        placed_reg
                    };

                    group_regs.push(placed);
                }

                // OR all group results together
                let mut result = group_regs[0];
                for &gr in &group_regs[1..] {
                    let or_reg = alloc_reg(next_reg, register_map, width);
                    new_insts.push(SIRInstruction::Binary(or_reg, result, BinaryOp::Or, gr));
                    result = or_reg;
                }

                // Replace Concat with identity from result
                new_insts.push(SIRInstruction::Unary(dst, UnaryOp::Ident, result));

                // Insert all new instructions at inst_idx, remove the Concat
                instructions.splice(inst_idx..=inst_idx, new_insts);
            }
        }
    }

    true
}
