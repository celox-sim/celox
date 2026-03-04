use crate::HashMap;
use crate::ir::*;
use crate::optimizer::PassOptions;
use super::pass_manager::ExecutionUnitPass;

pub(super) struct BitExtractPeepholePass;

impl ExecutionUnitPass for BitExtractPeepholePass {
    fn name(&self) -> &'static str {
        "bit_extract_peephole"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        for block in eu.blocks.values_mut() {
            optimize_bit_extracts(&mut block.instructions, &mut eu.register_map);
        }
    }
}

/// Try to extract a u64 value from a SIRValue that represents a 2-state constant.
fn sir_value_to_u64(val: &SIRValue) -> Option<u64> {
    if !val.mask.to_u64_digits().is_empty() {
        return None; // 4-state value
    }
    let digits = val.payload.to_u64_digits();
    match digits.len() {
        0 => Some(0),
        1 => Some(digits[0]),
        _ => None,
    }
}

/// Check if a value is a power-of-two-minus-one mask: (1 << W) - 1.
/// Returns W if so.
fn mask_width(mask_val: u64) -> Option<usize> {
    if mask_val == 0 {
        return None;
    }
    // mask_val must be a contiguous run of 1-bits from bit 0
    // i.e. mask_val + 1 must be a power of 2 (or mask_val == u64::MAX for w=64)
    let w = mask_val.count_ones() as usize;
    let expected = if w >= 64 {
        u64::MAX
    } else {
        (1u64 << w) - 1
    };
    if mask_val == expected {
        Some(w)
    } else {
        None
    }
}

fn optimize_bit_extracts(
    instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    register_map: &mut HashMap<RegisterId, RegisterType>,
) {
    if instructions.len() < 2 {
        return;
    }

    // Build def map: register -> instruction index
    let mut def_map: HashMap<RegisterId, usize> = HashMap::default();
    for (idx, inst) in instructions.iter().enumerate() {
        match inst {
            SIRInstruction::Imm(dst, _)
            | SIRInstruction::Binary(dst, _, _, _)
            | SIRInstruction::Unary(dst, _, _)
            | SIRInstruction::Load(dst, _, _, _)
            | SIRInstruction::Concat(dst, _) => {
                def_map.insert(*dst, idx);
            }
            _ => {}
        }
    }

    // Collect replacements: (and_inst_idx) -> replacement Load instruction
    struct Replacement {
        and_idx: usize,
        load_inst: SIRInstruction<RegionedAbsoluteAddr>,
        result_reg: RegisterId,
        result_width: usize,
        // Dead instruction indices to no-op (Shr + Imm(shift) + Imm(mask))
        dead_indices: Vec<usize>,
    }

    let mut replacements: Vec<Replacement> = Vec::new();

    for (idx, inst) in instructions.iter().enumerate() {
        // Pattern: r_result = Binary(_, r_shifted, And, r_mask)
        let SIRInstruction::Binary(r_result, r_shifted, BinaryOp::And, r_mask) = inst else {
            continue;
        };

        // r_mask must be Imm((1<<W)-1)
        let Some(&mask_def_idx) = def_map.get(r_mask) else {
            continue;
        };
        let SIRInstruction::Imm(_, ref mask_val) = instructions[mask_def_idx] else {
            continue;
        };
        let Some(mask_u64) = sir_value_to_u64(mask_val) else {
            continue;
        };
        let Some(w) = mask_width(mask_u64) else {
            continue;
        };

        // r_shifted must be Binary(_, r_src, Shr, r_shift)
        let Some(&shifted_def_idx) = def_map.get(r_shifted) else {
            continue;
        };
        let SIRInstruction::Binary(_, r_src, BinaryOp::Shr, r_shift) =
            &instructions[shifted_def_idx]
        else {
            continue;
        };

        // r_shift must be Imm(K)
        let Some(&shift_def_idx) = def_map.get(r_shift) else {
            continue;
        };
        let SIRInstruction::Imm(_, ref shift_val) = instructions[shift_def_idx] else {
            continue;
        };
        let Some(k) = sir_value_to_u64(shift_val) else {
            continue;
        };
        let k = k as usize;

        // r_src must be defined by a Load(_, addr, Static(base), N)
        let Some(&src_def_idx) = def_map.get(r_src) else {
            continue;
        };
        let SIRInstruction::Load(_, ref addr, SIROffset::Static(base), n) =
            instructions[src_def_idx]
        else {
            continue;
        };

        // Bounds check: base + k + w <= base + n (i.e., k + w <= n)
        if k + w > n {
            continue;
        }

        let new_load = SIRInstruction::Load(
            *r_result,
            addr.clone(),
            SIROffset::Static(base + k),
            w,
        );

        let mut dead = Vec::new();
        dead.push(shifted_def_idx); // Shr instruction
        dead.push(shift_def_idx); // Imm(K)
        dead.push(mask_def_idx); // Imm(mask)

        replacements.push(Replacement {
            and_idx: idx,
            load_inst: new_load,
            result_reg: *r_result,
            result_width: w,
            dead_indices: dead,
        });
    }

    if replacements.is_empty() {
        return;
    }

    // Collect all indices that are dead or replaced
    // But we must be careful: a "dead" instruction may be used by other instructions
    // that we are NOT replacing. Build use counts to check.
    let mut use_count: HashMap<RegisterId, usize> = HashMap::default();
    for inst in instructions.iter() {
        match inst {
            SIRInstruction::Binary(_, lhs, _, rhs) => {
                *use_count.entry(*lhs).or_default() += 1;
                *use_count.entry(*rhs).or_default() += 1;
            }
            SIRInstruction::Unary(_, _, src) => {
                *use_count.entry(*src).or_default() += 1;
            }
            SIRInstruction::Store(_, SIROffset::Dynamic(off), _, src, _) => {
                *use_count.entry(*off).or_default() += 1;
                *use_count.entry(*src).or_default() += 1;
            }
            SIRInstruction::Store(_, SIROffset::Static(_), _, src, _) => {
                *use_count.entry(*src).or_default() += 1;
            }
            SIRInstruction::Load(_, _, SIROffset::Dynamic(off), _) => {
                *use_count.entry(*off).or_default() += 1;
            }
            SIRInstruction::Concat(_, args) => {
                for arg in args {
                    *use_count.entry(*arg).or_default() += 1;
                }
            }
            _ => {}
        }
    }

    // For each replacement, decrement use counts for the operands we're removing
    // and only mark instructions as dead if their result has no remaining uses.
    let mut dead_set = std::collections::HashSet::new();
    let mut replaced_set = std::collections::HashSet::new();

    for repl in &replacements {
        replaced_set.insert(repl.and_idx);

        // The And instruction used r_shifted and r_mask — decrement their use counts
        if let SIRInstruction::Binary(_, r_shifted, _, r_mask) = &instructions[repl.and_idx] {
            *use_count.entry(*r_shifted).or_default() = use_count
                .get(r_shifted)
                .copied()
                .unwrap_or(0)
                .saturating_sub(1);
            *use_count.entry(*r_mask).or_default() = use_count
                .get(r_mask)
                .copied()
                .unwrap_or(0)
                .saturating_sub(1);
        }
    }

    // Now check which "dead" instructions are actually dead (use count == 0)
    for repl in &replacements {
        for &dead_idx in &repl.dead_indices {
            let def_reg = match &instructions[dead_idx] {
                SIRInstruction::Imm(dst, _)
                | SIRInstruction::Binary(dst, _, _, _)
                | SIRInstruction::Load(dst, _, _, _) => *dst,
                _ => continue,
            };
            if use_count.get(&def_reg).copied().unwrap_or(0) == 0 {
                // Also decrement use counts for operands of this dead instruction
                if let SIRInstruction::Binary(_, lhs, _, rhs) = &instructions[dead_idx] {
                    *use_count.entry(*lhs).or_default() = use_count
                        .get(lhs)
                        .copied()
                        .unwrap_or(0)
                        .saturating_sub(1);
                    *use_count.entry(*rhs).or_default() = use_count
                        .get(rhs)
                        .copied()
                        .unwrap_or(0)
                        .saturating_sub(1);
                }
                dead_set.insert(dead_idx);
            }
        }
    }

    // Build replacement map
    let mut replacement_map: HashMap<usize, SIRInstruction<RegionedAbsoluteAddr>> =
        HashMap::default();
    for repl in replacements {
        replacement_map.insert(repl.and_idx, repl.load_inst);
        register_map.insert(repl.result_reg, RegisterType::Logic { width: repl.result_width });
    }

    // Rebuild instructions
    let mut out = Vec::with_capacity(instructions.len());
    for (i, inst) in instructions.drain(..).enumerate() {
        if dead_set.contains(&i) {
            continue;
        }
        if let Some(new_inst) = replacement_map.remove(&i) {
            out.push(new_inst);
        } else {
            out.push(inst);
        }
    }

    *instructions = out;
}
