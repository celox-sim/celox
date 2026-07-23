use crate::HashSet;
use crate::ir::*;

/// Remove stores from `eval_comb` whose target addresses are not live.
///
/// A store's address is considered live if:
/// - It is in `externally_live` (user-specified observable signals), OR
/// - Any execution unit Loads from it (or Commits from it), OR
/// - It has a dynamic offset (conservative), OR
/// - The store has non-empty triggers (edge-detection side effect), OR
/// - The store has non-empty comb capture sites (observer activation side effect).
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
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
        .chain(program.apply_ffs.values().flat_map(|units| units.iter()));

    for eu in all_eus {
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Load(_, addr, SIROffset::Static(_), _) => {
                        loaded_addrs.insert(addr.absolute_addr());
                    }
                    SIRInstruction::Load(
                        _,
                        addr,
                        SIROffset::Dynamic(_) | SIROffset::Element { .. },
                        _,
                    ) => {
                        let key = addr.absolute_addr();
                        loaded_addrs.insert(key);
                        dynamic_addrs.insert(key);
                    }
                    SIRInstruction::Commit(src, _, SIROffset::Static(_), _, _) => {
                        loaded_addrs.insert(src.absolute_addr());
                    }
                    SIRInstruction::Commit(
                        src,
                        _,
                        SIROffset::Dynamic(_) | SIROffset::Element { .. },
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
    for eu in program.eval_comb.iter_mut() {
        for block in eu.blocks.values_mut() {
            block.instructions.retain(|inst| {
                match inst {
                    SIRInstruction::Store(
                        addr,
                        SIROffset::Static(_),
                        _,
                        _,
                        triggers,
                        comb_capture_sites,
                    ) if triggers.is_empty() && comb_capture_sites.is_empty() => {
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

/// Remove unread combinational publications from a fused comb/FF clone.
///
/// `tick_deferred_comb` marks simulator state dirty immediately after this
/// function returns. External signal reads therefore settle `eval_comb` before
/// observing state, so a comb-prefix Store is not an exit root merely because
/// the standalone combinational function publishes the same signal.
///
/// This initial subset is deliberately stronger than address-only program
/// DSE, but weaker than full MemorySSA DSE: it removes a static, effect-free
/// Store only when no instruction anywhere in the fused function reads an
/// overlapping static range. Any dynamic access to the same object blocks the
/// rewrite. Stores in the FF suffix remain persistent-state publications.
pub(crate) fn eliminate_unread_fused_comb_stores(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    ff_entry: BlockId,
) -> Result<usize, String> {
    let dead = super::fused_state_feasibility::dirty_exit_dead_comb_stores(eu, ff_entry)
        .map_err(|error| error.to_string())?;
    let removed = dead.len();
    for (&block_id, block) in &mut eu.blocks {
        let mut instruction = 0usize;
        block.instructions.retain(|_| {
            let keep = !dead.contains(&(block_id, instruction));
            instruction += 1;
            keep
        });
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{InstanceId, RegisterType};
    use veryl_analyzer::ir::VarId;

    fn address(instance: usize) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: InstanceId(instance),
            var_id: VarId::default(),
        }
    }

    fn fused_unit(
        comb_instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        ff_instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let blocks = [
            (
                BlockId(0),
                BasicBlock {
                    id: BlockId(0),
                    params: Vec::new(),
                    instructions: comb_instructions,
                    terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
                },
            ),
            (
                BlockId(1),
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: ff_instructions,
                    terminator: SIRTerminator::Return,
                },
            ),
        ]
        .into_iter()
        .collect();
        let register_map = (0..8)
            .map(|register| {
                (
                    RegisterId(register),
                    RegisterType::Bit {
                        width: 64,
                        signed: false,
                    },
                )
            })
            .collect();
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    fn store(
        object: usize,
        start: usize,
        width: usize,
        source: usize,
    ) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Store(
            address(object),
            SIROffset::Static(start),
            width,
            RegisterId(source),
            Vec::new(),
            Vec::new(),
        )
    }

    #[test]
    fn fused_dse_removes_only_unread_comb_ranges() {
        let mut eu = fused_unit(
            vec![store(0, 0, 8, 0), store(1, 0, 8, 1), store(2, 0, 8, 2)],
            vec![
                SIRInstruction::Load(RegisterId(3), address(1), SIROffset::Static(0), 8),
                SIRInstruction::Load(RegisterId(4), address(2), SIROffset::Static(8), 8),
            ],
        );

        assert_eq!(
            eliminate_unread_fused_comb_stores(&mut eu, BlockId(1)).unwrap(),
            2
        );
        assert_eq!(eu.blocks[&BlockId(0)].instructions, vec![store(1, 0, 8, 1)]);
    }

    #[test]
    fn fused_dse_keeps_effectful_and_dynamically_aliased_comb_stores() {
        let mut effectful = store(0, 0, 8, 0);
        let SIRInstruction::Store(_, _, _, _, _, captures) = &mut effectful else {
            unreachable!();
        };
        captures.push(7);
        let mut eu = fused_unit(
            vec![effectful.clone(), store(1, 0, 8, 1)],
            vec![SIRInstruction::Load(
                RegisterId(2),
                address(1),
                SIROffset::Dynamic(RegisterId(3)),
                8,
            )],
        );

        assert_eq!(
            eliminate_unread_fused_comb_stores(&mut eu, BlockId(1)).unwrap(),
            0
        );
        assert_eq!(
            eu.blocks[&BlockId(0)].instructions,
            vec![effectful, store(1, 0, 8, 1)]
        );
    }

    #[test]
    fn fused_dse_uses_reaching_versions_not_any_read_of_the_range() {
        let mut eu = fused_unit(
            vec![
                store(0, 0, 8, 0),
                SIRInstruction::Load(RegisterId(1), address(0), SIROffset::Static(0), 8),
                store(0, 0, 8, 2),
            ],
            Vec::new(),
        );

        assert_eq!(
            eliminate_unread_fused_comb_stores(&mut eu, BlockId(1)).unwrap(),
            1
        );
        assert_eq!(
            eu.blocks[&BlockId(0)].instructions,
            vec![
                store(0, 0, 8, 0),
                SIRInstruction::Load(RegisterId(1), address(0), SIROffset::Static(0), 8),
            ]
        );
    }

    #[test]
    fn fused_dse_keeps_prior_range_needed_by_effectful_store() {
        let mut effectful = store(0, 0, 8, 1);
        let SIRInstruction::Store(_, _, _, _, _, captures) = &mut effectful else {
            unreachable!();
        };
        captures.push(9);
        let mut eu = fused_unit(vec![store(0, 0, 8, 0), effectful], Vec::new());

        assert_eq!(
            eliminate_unread_fused_comb_stores(&mut eu, BlockId(1)).unwrap(),
            0
        );
    }

    #[test]
    fn fused_dse_keeps_comb_range_which_survives_a_partial_ff_publication() {
        let mut eu = fused_unit(vec![store(0, 0, 8, 0)], vec![store(0, 8, 8, 1)]);

        assert_eq!(
            eliminate_unread_fused_comb_stores(&mut eu, BlockId(1)).unwrap(),
            0
        );
    }
}
