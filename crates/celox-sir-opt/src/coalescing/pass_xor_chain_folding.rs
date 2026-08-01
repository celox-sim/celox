//! XOR chain folding: replace Binary XOR chains of single-bit Slices
//! from the same source with And(source, mask) + Unary::Xor.
//!
//! Pattern:
//!   bit_a = Slice(src, A, 1)
//!   bit_b = Slice(src, B, 1)
//!   xor1 = Binary(bit_a, Xor, bit_b)
//!   bit_c = Slice(src, C, 1)
//!   xor2 = Binary(xor1, Xor, bit_c)
//!   ...
//!
//! Replacement:
//!   mask = Imm((1<<A) | (1<<B) | (1<<C) | ...)
//!   masked = Binary(src, And, mask)
//!   result = Unary(Xor, masked)   // ISel: popcnt + and 1

use super::pass_manager::ExecutionUnitPass;
use super::shared::{collect_all_used_registers, def_reg};
use crate::HashMap;
use crate::ir::*;
use crate::optimizer::PassOptions;
use num_bigint::BigUint;

pub(super) struct XorChainFoldingPass;

impl ExecutionUnitPass for XorChainFoldingPass {
    fn name(&self) -> &'static str {
        "xor_chain_folding"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        let mut any_changed = false;
        let mut max_reg = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);

        for block in eu.blocks.values_mut() {
            if fold_xor_chains(&mut block.instructions, &mut eu.register_map, &mut max_reg) {
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

/// Try to fold XOR chains into And + Unary::Xor.
fn fold_xor_chains(
    instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    next_reg: &mut usize,
) -> bool {
    // Build def map
    let mut defs: HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>> = HashMap::default();
    for inst in instructions.iter() {
        if let Some(d) = def_reg(inst) {
            defs.insert(d, inst.clone());
        }
    }

    let mut replacements: Vec<(usize, RegisterId, RegisterId, u64, usize)> = Vec::new();
    // (inst_index, dst_reg, source_reg, mask, source_width)

    for (idx, inst) in instructions.iter().enumerate() {
        // Look for Binary XOR that's the root of a chain
        let SIRInstruction::Binary(dst, lhs, BinaryOp::Xor, rhs) = inst else {
            continue;
        };

        // Collect all single-bit Slice positions from this XOR chain
        let mut bits: Vec<usize> = Vec::new();
        let mut source: Option<RegisterId> = None;

        if collect_xor_bits(*lhs, &defs, &mut bits, &mut source)
            && collect_xor_bits(*rhs, &defs, &mut bits, &mut source)
            && bits.len() >= 3
        {
            if let Some(src) = source {
                let src_width = register_map.get(&src).map(|t| t.width()).unwrap_or(64);
                let mut mask: u64 = 0;
                for &pos in &bits {
                    if pos < 64 {
                        mask |= 1u64 << pos;
                    }
                }
                if mask != 0 {
                    replacements.push((idx, *dst, src, mask, src_width));
                }
            }
        }
    }

    if replacements.is_empty() {
        return false;
    }

    // Apply replacements: for each, insert And + Unary::Xor before the XOR chain root
    // and replace the root XOR with an identity alias.
    // Process in reverse to preserve indices.
    for (idx, dst, src, mask, src_width) in replacements.into_iter().rev() {
        let mask_width = src_width.min(64);

        // Create fresh registers
        *next_reg += 1;
        let mask_reg = RegisterId(*next_reg);
        register_map.insert(
            mask_reg,
            RegisterType::Bit {
                width: mask_width,
                signed: false,
            },
        );

        *next_reg += 1;
        let masked_reg = RegisterId(*next_reg);
        register_map.insert(
            masked_reg,
            RegisterType::Bit {
                width: mask_width,
                signed: false,
            },
        );

        // Insert: mask = Imm(mask_value)
        let mask_value = SIRValue {
            payload: BigUint::from(mask),
            mask: BigUint::ZERO,
        };
        instructions.insert(idx, SIRInstruction::Imm(mask_reg, mask_value));

        // Insert: masked = Binary(src, And, mask_reg)
        instructions.insert(
            idx + 1,
            SIRInstruction::Binary(masked_reg, src, BinaryOp::And, mask_reg),
        );

        // Replace the original XOR with Unary::Xor of masked
        instructions[idx + 2] = SIRInstruction::Unary(dst, UnaryOp::Xor, masked_reg);
    }

    true
}

/// Recursively collect bit positions from a XOR chain.
/// Returns false if the chain contains non-Slice or mixed-source nodes.
fn collect_xor_bits(
    reg: RegisterId,
    defs: &HashMap<RegisterId, SIRInstruction<RegionedAbsoluteAddr>>,
    bits: &mut Vec<usize>,
    source: &mut Option<RegisterId>,
) -> bool {
    let Some(def) = defs.get(&reg) else {
        return false;
    };

    match def {
        SIRInstruction::Slice(_, src, offset, 1) => {
            // Single-bit slice: record bit position
            match source {
                Some(s) if *s != *src => return false,
                None => *source = Some(*src),
                _ => {}
            }
            bits.push(*offset);
            true
        }
        SIRInstruction::Load(_, addr, SIROffset::Static(offset), 1) => {
            // Single-bit load: treat addr as the source identity.
            // We need a RegisterId for the source, but Load uses an address.
            // Use the Load's destination as a marker — this only works if
            // all loads are from the same address (same variable).
            // Skip for now: Load-based XOR chains need address comparison.
            let _ = (addr, offset);
            false
        }
        SIRInstruction::Binary(_, lhs, BinaryOp::Xor, rhs) => {
            collect_xor_bits(*lhs, defs, bits, source) && collect_xor_bits(*rhs, defs, bits, source)
        }
        // Look through Unary::Ident (identity/cast)
        SIRInstruction::Unary(_, UnaryOp::Ident, src) => collect_xor_bits(*src, defs, bits, source),
        _ => false,
    }
}
