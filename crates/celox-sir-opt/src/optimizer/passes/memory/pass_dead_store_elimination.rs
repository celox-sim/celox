use crate::ir::*;
use crate::{HashSet, OptimizationContext};

/// Remove stores from `eval_comb` and its lane-partitioned alternative whose
/// target addresses are not live.
///
/// A store's address is considered live if:
/// - It is in `externally_live` (user-specified observable signals), OR
/// - Any execution unit Loads from it (or Commits from it), OR
/// - It has a dynamic offset (conservative), OR
/// - The store has non-empty triggers (edge-detection side effect), OR
/// - The store has non-empty comb capture sites (observer activation side effect).
pub(crate) fn eliminate_dead_stores(
    program: &mut OptimizationContext,
    externally_live: &HashSet<AbsoluteAddr>,
) {
    // 1. Collect all addresses loaded across ALL execution units.
    let mut loaded_addrs: HashSet<AbsoluteAddr> = HashSet::default();
    let mut dynamic_addrs: HashSet<AbsoluteAddr> = HashSet::default();

    let all_eus = program
        .sir
        .eval_comb
        .iter()
        .chain(
            program
                .sir
                .eval_apply_ffs
                .values()
                .flat_map(|units| units.iter()),
        )
        .chain(
            program
                .sir
                .eval_comb_apply_ffs
                .values()
                .flat_map(|units| units.iter()),
        )
        .chain(
            program
                .sir
                .eval_only_ffs
                .values()
                .flat_map(|units| units.iter()),
        )
        .chain(
            program
                .sir
                .apply_ffs
                .values()
                .flat_map(|units| units.iter()),
        )
        .chain(
            program
                .sir
                .parallel
                .iter()
                .flat_map(|parallel| parallel.units().map(|unit| &unit.unit)),
        );

    for eu in all_eus {
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Load(_, addr, offset, _)
                        if offset.constant_bit_offset().is_some() =>
                    {
                        loaded_addrs.insert(addr.absolute_addr());
                    }
                    SIRInstruction::Load(
                        _,
                        addr,
                        SIROffset::Dynamic(_)
                        | SIROffset::Element { .. }
                        | SIROffset::ElementRun { .. },
                        _,
                    ) => {
                        let key = addr.absolute_addr();
                        loaded_addrs.insert(key);
                        dynamic_addrs.insert(key);
                    }
                    SIRInstruction::Commit(src, _, offset, _, _)
                        if offset.constant_bit_offset().is_some() =>
                    {
                        loaded_addrs.insert(src.absolute_addr());
                    }
                    SIRInstruction::Commit(
                        src,
                        _,
                        SIROffset::Dynamic(_)
                        | SIROffset::Element { .. }
                        | SIROffset::ElementRun { .. },
                        _,
                        _,
                    ) => {
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
    let parallel_comb = program
        .sir
        .parallel
        .iter_mut()
        .flat_map(|parallel| parallel.eval_comb.iter_mut().map(|unit| &mut unit.unit));
    for eu in program.sir.eval_comb.iter_mut().chain(parallel_comb) {
        for block in eu.blocks.values_mut() {
            block.instructions.retain(|inst| {
                match inst {
                    SIRInstruction::Store(addr, offset, _, _, triggers, comb_capture_sites)
                        if offset.constant_bit_offset().is_some()
                            && triggers.is_empty()
                            && comb_capture_sites.is_empty() =>
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
