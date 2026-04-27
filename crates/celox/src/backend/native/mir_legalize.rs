use std::collections::{HashMap, HashSet};

use super::mir::*;

pub fn legalize(func: &mut MFunction) {
    eliminate_trivial_phis(func);
}

fn eliminate_trivial_phis(func: &mut MFunction) {
    let mut aliases: HashMap<VReg, VReg> = HashMap::new();

    for block in &func.blocks {
        for phi in &block.phis {
            if phi.sources.is_empty() {
                continue;
            }
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

    let mut resolved: HashMap<VReg, VReg> = HashMap::with_capacity(aliases.len());
    for (&dst, &src) in &aliases {
        let mut target = src;
        let mut seen = HashSet::from([dst]);
        let mut cyclic = false;
        while let Some(&next) = aliases.get(&target) {
            if !seen.insert(target) || !seen.insert(next) {
                cyclic = true;
                break;
            }
            target = next;
        }
        if !cyclic {
            resolved.insert(dst, target);
        }
    }
    aliases = resolved;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_trivial_phi_cycles_intact() {
        let mut vregs = VRegAllocator::new();
        let v0 = vregs.alloc();
        let v1 = vregs.alloc();
        let spill_descs = vec![SpillDesc::transient(), SpillDesc::transient()];
        let mut func = MFunction::new(vregs, spill_descs);

        let mut block = MBlock::new(BlockId(0));
        block.phis.push(PhiNode {
            dst: v0,
            sources: vec![(BlockId(0), v1)],
        });
        block.phis.push(PhiNode {
            dst: v1,
            sources: vec![(BlockId(0), v0)],
        });
        block.push(MInst::Return);
        func.push_block(block);

        legalize(&mut func);

        assert_eq!(func.blocks[0].phis.len(), 2);
        assert_eq!(func.blocks[0].phis[0].sources[0].1, v1);
        assert_eq!(func.blocks[0].phis[1].sources[0].1, v0);
    }
}
