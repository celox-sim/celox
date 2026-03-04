use crate::ir::*;
use crate::HashSet;

/// Remove stores from `eval_comb` whose target addresses are not live.
///
/// A store's address is considered live if:
/// - It is in `externally_live` (user-specified observable signals), OR
/// - Any execution unit Loads from it (or Commits from it), OR
/// - It has a dynamic offset (conservative), OR
/// - The store has non-empty triggers (edge-detection side effect).
pub(crate) fn eliminate_dead_stores(
    program: &mut Program,
    externally_live: &HashSet<AbsoluteAddr>,
) {
    // 1. Collect all addresses loaded across ALL execution units.
    let mut loaded_addrs: HashSet<AbsoluteAddr> = HashSet::default();
    let mut dynamic_addrs: HashSet<AbsoluteAddr> = HashSet::default();

    let all_eus = program
        .eval_comb
        .iter()
        .chain(
            program
                .eval_apply_ffs
                .values()
                .flat_map(|units| units.iter()),
        )
        .chain(
            program
                .eval_only_ffs
                .values()
                .flat_map(|units| units.iter()),
        )
        .chain(
            program
                .apply_ffs
                .values()
                .flat_map(|units| units.iter()),
        );

    for eu in all_eus {
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Load(_, addr, SIROffset::Static(_), _) => {
                        loaded_addrs.insert(addr.absolute_addr());
                    }
                    SIRInstruction::Load(_, addr, SIROffset::Dynamic(_), _) => {
                        let key = addr.absolute_addr();
                        loaded_addrs.insert(key);
                        dynamic_addrs.insert(key);
                    }
                    SIRInstruction::Commit(src, _, SIROffset::Static(_), _, _) => {
                        loaded_addrs.insert(src.absolute_addr());
                    }
                    SIRInstruction::Commit(src, _, SIROffset::Dynamic(_), _, _) => {
                        let key = src.absolute_addr();
                        loaded_addrs.insert(key);
                        dynamic_addrs.insert(key);
                    }
                    _ => {}
                }
            }
        }
    }

    // 2. Remove dead stores from eval_comb.
    for eu in program.eval_comb.iter_mut() {
        for block in eu.blocks.values_mut() {
            block.instructions.retain(|inst| {
                match inst {
                    SIRInstruction::Store(addr, SIROffset::Static(_), _, _, triggers)
                        if triggers.is_empty() =>
                    {
                        let abs = addr.absolute_addr();
                        externally_live.contains(&abs)
                            || loaded_addrs.contains(&abs)
                            || dynamic_addrs.contains(&abs)
                    }
                    // Keep stores with dynamic offsets or triggers unconditionally.
                    _ => true,
                }
            });
        }
    }
}
