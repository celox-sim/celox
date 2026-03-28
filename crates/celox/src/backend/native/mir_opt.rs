//! MIR optimization passes: run between ISel and regalloc.
//!
//! - Copy propagation: `v2 = mov v1` → replace all uses of v2 with v1
//! - Dead code elimination: remove instructions whose defs are unused

use std::collections::HashMap;

use super::mir::*;

/// Run all MIR optimization passes.
pub fn optimize(func: &mut MFunction) {
    copy_propagate(func);
    dead_code_eliminate(func);
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
