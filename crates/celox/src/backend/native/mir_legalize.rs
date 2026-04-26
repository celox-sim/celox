use std::collections::HashMap;

use super::mir::*;

pub fn legalize(func: &mut MFunction) {
    eliminate_trivial_phis(func);
}

fn eliminate_trivial_phis(func: &mut MFunction) {
    let mut aliases: HashMap<VReg, VReg> = HashMap::new();

    for block in &func.blocks {
        for phi in &block.phis {
            let mut unique_src = None;
            let mut trivial = true;
            for (_, src) in &phi.sources {
                match unique_src {
                    None => unique_src = Some(*src),
                    Some(existing) if existing == *src => {}
                    Some(_) => {
                        trivial = false;
                        break;
                    }
                }
            }
            if trivial {
                if let Some(src) = unique_src {
                    if src != phi.dst {
                        aliases.insert(phi.dst, src);
                    }
                }
            }
        }
    }

    if aliases.is_empty() {
        return;
    }

    for block in &mut func.blocks {
        for inst in &mut block.insts {
            rewrite_uses(inst, &aliases);
        }
        for phi in &mut block.phis {
            for (_, src) in &mut phi.sources {
                if let Some(&alias) = aliases.get(src) {
                    *src = alias;
                }
            }
        }
        block
            .phis
            .retain(|phi| !matches!(aliases.get(&phi.dst), Some(src) if *src != phi.dst));
    }
}

fn rewrite_uses(inst: &mut MInst, aliases: &HashMap<VReg, VReg>) {
    let current_uses = inst.uses();
    for u in current_uses {
        if let Some(&target) = aliases.get(&u) {
            inst.rewrite_use(u, target);
        }
    }
}
