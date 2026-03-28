//! MIR optimization passes: run between ISel and regalloc.
//!
//! - Copy propagation: `v2 = mov v1` → replace all uses of v2 with v1
//! - Dead code elimination: remove instructions whose defs are unused

use std::collections::HashMap;

use super::mir::*;

/// Run all MIR optimization passes.
pub fn optimize(func: &mut MFunction) {
    constant_dedup(func);
    copy_propagate(func);
    fold_xor_chain_to_pext(func);
    dead_code_eliminate(func);
}

/// Fold XOR chains of single-bit extractions from the same source into
/// PEXT + POPCNT + AND 1.
///
/// Pattern: `(src >> a) & 1 ^ (src >> b) & 1 ^ ...` where all extractions
/// come from the same source register.
///
/// Replacement: `pext(src, mask) → popcnt → and 1` where
/// `mask = (1 << a) | (1 << b) | ...`
fn fold_xor_chain_to_pext(func: &mut MFunction) {
    // Build def map: VReg → instruction (cloned to avoid borrowing func)
    let mut defs: HashMap<VReg, MInst> = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let Some(d) = inst.def() {
                defs.insert(d, inst.clone());
            }
        }
    }

    // For each block, scan for Xor instructions and try to fold
    for block in &mut func.blocks {
        let mut replacements: Vec<(usize, Vec<MInst>)> = Vec::new();

        for (inst_idx, inst) in block.insts.iter().enumerate() {
            // Look for: v = xor a, b  where result is 1-bit (used with and 1)
            let MInst::Xor { dst, lhs, rhs } = inst else { continue };

            // Try to collect the full XOR chain and extract bit positions
            let mut bits: Vec<(VReg, u64)> = Vec::new();
            let mut source_reg: Option<VReg> = None;

            let ok = collect_xor_chain_bits(*dst, *lhs, *rhs, &defs, &mut bits, &mut source_reg);
            if !ok {
                continue;
            }

            // Need at least 3 bits to be worth the PEXT overhead
            let Some(src) = source_reg else { continue };
            if bits.len() < 3 { continue; }

            // Build mask from bit positions
            let mut mask_val: u64 = 0;
            for &(_, pos) in &bits {
                if pos >= 64 { continue; } // skip wide
                mask_val |= 1u64 << pos;
            }
            if mask_val == 0 { continue; }

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
                MInst::LoadImm { dst: mask_vreg, value: mask_val },
                MInst::Pext { dst: pext_vreg, src, mask: mask_vreg },
                MInst::Popcnt { dst: popcnt_vreg, src: pext_vreg },
                MInst::AndImm { dst: *dst, src: popcnt_vreg, imm: 1 },
            ];
            replacements.push((inst_idx, new_insts));
        }

        // Apply replacements in reverse order (to preserve indices)
        for (idx, new_insts) in replacements.into_iter().rev() {
            block.insts.splice(idx..=idx, new_insts);
        }
    }
}

/// Recursively collect single-bit extractions from a XOR chain.
/// Returns true if the entire chain consists of single-bit extractions
/// from the same source register.
fn collect_xor_chain_bits(
    _vreg: VReg,
    lhs: VReg,
    rhs: VReg,
    defs: &HashMap<VReg, MInst>,
    bits: &mut Vec<(VReg, u64)>,
    source_reg: &mut Option<VReg>,
) -> bool {
    // Try to extract a bit from each operand
    for &operand in &[lhs, rhs] {
        if let Some(def_inst) = defs.get(&operand) {
            match def_inst {
                // Pattern: v = xor a, b (recursive)
                MInst::Xor { lhs: l2, rhs: r2, .. } => {
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
                MInst::AndImm { src: and_src, imm: 1, .. } => {
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
                            MInst::Xor { lhs: l2, rhs: r2, .. } => {
                                if !collect_xor_chain_bits(*and_src, *l2, *r2, defs, bits, source_reg) {
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

/// Constant deduplication: merge LoadImm instructions with the same value
/// into a single VReg. Reduces register pressure and instruction count.
fn constant_dedup(func: &mut MFunction) {
    let mut aliases: HashMap<VReg, VReg> = HashMap::new();
    // Map from constant value → canonical VReg
    let mut const_map: HashMap<u64, VReg> = HashMap::new();

    for block in &func.blocks {
        for inst in &block.insts {
            if let MInst::LoadImm { dst, value } = inst {
                if let Some(&canonical) = const_map.get(value) {
                    aliases.insert(*dst, canonical);
                } else {
                    const_map.insert(*value, *dst);
                }
            }
        }
    }

    if aliases.is_empty() {
        return;
    }

    // Apply aliases
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            let current_uses = inst.uses();
            for u in current_uses {
                if let Some(&target) = aliases.get(&u) {
                    inst.rewrite_use(u, target);
                }
            }
        }
        for phi in &mut block.phis {
            for (_, src) in &mut phi.sources {
                if let Some(&a) = aliases.get(src) {
                    *src = a;
                }
            }
        }
    }

    // Remove duplicated LoadImm
    for block in &mut func.blocks {
        block.insts.retain(|inst| {
            if let MInst::LoadImm { dst, .. } = inst {
                !aliases.contains_key(dst)
            } else {
                true
            }
        });
    }
}

/// Copy propagation: for each `Mov { dst, src }`, replace all uses of dst
/// with src throughout the function. Then remove the Mov.
fn copy_propagate(func: &mut MFunction) {
    // Build alias map: dst → src (transitively resolved)
    let mut aliases: HashMap<VReg, VReg> = HashMap::new();

    for block in &func.blocks {
        for inst in &block.insts {
            if let MInst::Mov { dst, src } = inst {
                // Resolve transitively: if src is itself an alias, follow the chain
                let mut target = *src;
                while let Some(&next) = aliases.get(&target) {
                    target = next;
                }
                aliases.insert(*dst, target);
            }
        }
    }

    if aliases.is_empty() {
        return;
    }

    // Apply aliases to all instructions
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            rewrite_uses(inst, &aliases);
        }
        // Also rewrite phi sources
        for phi in &mut block.phis {
            for (_pred, src) in &mut phi.sources {
                if let Some(&a) = aliases.get(src) {
                    *src = a;
                }
            }
        }
    }

    // Remove Mov instructions that are now identity (dst == src after alias resolution)
    // or whose dst is aliased away
    for block in &mut func.blocks {
        block.insts.retain(|inst| {
            if let MInst::Mov { dst, src } = inst {
                // Keep only if dst is not aliased (it's still needed)
                if aliases.contains_key(dst) {
                    return false; // Remove: dst was aliased to src
                }
                if dst == src {
                    return false; // Remove: identity mov
                }
            }
            true
        });
    }
}

/// Dead code elimination: remove instructions whose defs are never used.
fn dead_code_eliminate(func: &mut MFunction) {
    // Collect all used VRegs
    let mut used: std::collections::HashSet<VReg> = std::collections::HashSet::new();
    for block in &func.blocks {
        for inst in &block.insts {
            for u in inst.uses() {
                used.insert(u);
            }
        }
        for phi in &block.phis {
            for (_, src) in &phi.sources {
                used.insert(*src);
            }
        }
    }

    // Remove instructions whose def is not used (except side-effecting ones)
    for block in &mut func.blocks {
        block.insts.retain(|inst| {
            if let Some(def) = inst.def() {
                if !used.contains(&def) {
                    // Check if instruction has side effects
                    return matches!(
                        inst,
                        MInst::Store { .. }
                            | MInst::StoreIndexed { .. }
                            | MInst::Branch { .. }
                            | MInst::Jump { .. }
                            | MInst::Return
                            | MInst::ReturnError { .. }
                    );
                }
            }
            true
        });
    }
}

/// Rewrite all use operands in an instruction according to the alias map.
fn rewrite_uses(inst: &mut MInst, aliases: &HashMap<VReg, VReg>) {
    // Iterate over uses and rewrite any that appear in aliases
    let current_uses = inst.uses();
    for u in current_uses {
        if let Some(&target) = aliases.get(&u) {
            inst.rewrite_use(u, target);
        }
    }
}
