//! Eliminate working memory round-trip for independent FF patterns.
//!
//! A normal eval/apply EU does
//! `Seed(STABLE→WORKING) + compute + Store(WORKING) + Apply(WORKING→STABLE)`.
//! Dynamic FF arrays instead write a sparse next-state region and publish its
//! dirty chunks at the event tail.  When the complete event CFG proves that no
//! old STABLE observation or competing write occurs before publication, both
//! forms can write STABLE directly.

use crate::ir::*;

/// Eliminate the WORKING memory round-trip for independent variables.
///
/// `eu_boundary_blocks`: block IDs that start a new original EU (after SIR merge).
/// Empty for pre-merge (single EU) — all variables are trivially independent.
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
pub(crate) fn eliminate_working_round_trip(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    eu_boundary_blocks: &[BlockId],
) {
    use std::collections::HashMap;

    // Phase 1: Scan — collect info about each WORKING variable
    struct VarInfo {
        stable_addr: RegionedAbsoluteAddr,
        seed_locs: Vec<(BlockId, usize)>,  // Commit(STABLE→WORKING)
        apply_locs: Vec<(BlockId, usize)>, // Commit(WORKING→STABLE)
        has_dynamic: bool,
    }

    struct SparseVarInfo {
        stable_addr: Option<RegionedAbsoluteAddr>,
        has_store: bool,
        invalid_apply: bool,
    }

    let mut vars: HashMap<AbsoluteAddr, VarInfo> = HashMap::new();
    let mut sparse_vars: HashMap<AbsoluteAddr, SparseVarInfo> = HashMap::new();

    // Build block → EU index mapping (for cross-EU independence check)
    let block_to_eu: HashMap<BlockId, usize> = if eu_boundary_blocks.is_empty() {
        HashMap::new() // single EU, no mapping needed
    } else {
        let mut sorted_boundaries: Vec<BlockId> = eu_boundary_blocks.to_vec();
        sorted_boundaries.sort_by_key(|b| b.0);

        let mut mapping = HashMap::new();
        let mut all_blocks: Vec<BlockId> = eu.blocks.keys().copied().collect();
        all_blocks.sort_by_key(|b| b.0);

        let mut eu_idx = 0;
        let mut boundary_iter = sorted_boundaries.iter().peekable();
        for &bid in &all_blocks {
            if boundary_iter.peek().is_some_and(|&&b| b == bid) {
                eu_idx += 1;
                boundary_iter.next();
            }
            mapping.insert(bid, eu_idx);
        }
        mapping
    };

    // Track which EU indices access each WORKING variable (for independence check)
    let mut var_eu_access: HashMap<AbsoluteAddr, std::collections::HashSet<usize>> = HashMap::new();

    for (&bid, block) in &eu.blocks {
        let eu_idx = block_to_eu.get(&bid).copied().unwrap_or(0);

        for (ii, inst) in block.instructions.iter().enumerate() {
            match inst {
                SIRInstruction::Commit(src, dst, offset, _bits, triggers) => {
                    let abs = src.absolute_addr();
                    if src.region == STABLE_REGION && dst.region == WORKING_REGION {
                        // Seed: STABLE → WORKING
                        let has_dynamic = offset.is_dynamic();
                        let entry = vars.entry(abs).or_insert_with(|| VarInfo {
                            stable_addr: *src,
                            seed_locs: Vec::new(),
                            apply_locs: Vec::new(),
                            has_dynamic: false,
                        });
                        entry.seed_locs.push((bid, ii));
                        if has_dynamic {
                            entry.has_dynamic = true;
                        }
                        var_eu_access.entry(abs).or_default().insert(eu_idx);
                    } else if src.region == WORKING_REGION && dst.region == STABLE_REGION {
                        // Apply: WORKING → STABLE
                        let abs_w = src.absolute_addr();
                        let has_dynamic = offset.is_dynamic();
                        let entry = vars.entry(abs_w).or_insert_with(|| VarInfo {
                            stable_addr: *dst,
                            seed_locs: Vec::new(),
                            apply_locs: Vec::new(),
                            has_dynamic: false,
                        });
                        entry.apply_locs.push((bid, ii));
                        if has_dynamic {
                            entry.has_dynamic = true;
                        }
                        var_eu_access.entry(abs_w).or_default().insert(eu_idx);
                    } else if src.region == SPARSE_WORKING_REGION && dst.region == STABLE_REGION {
                        let invalid_apply = src.absolute_addr() != dst.absolute_addr()
                            || !matches!(offset, SIROffset::Static(0))
                            || !triggers.is_empty();
                        let entry = sparse_vars.entry(abs).or_insert(SparseVarInfo {
                            stable_addr: None,
                            has_store: false,
                            invalid_apply: false,
                        });
                        if entry.stable_addr.is_some_and(|stable| stable != *dst) {
                            entry.invalid_apply = true;
                        }
                        entry.stable_addr = Some(*dst);
                        entry.invalid_apply |= invalid_apply;
                    }
                }
                SIRInstruction::Load(_, addr, offset, _) if addr.region == WORKING_REGION => {
                    let abs = addr.absolute_addr();
                    if offset.is_dynamic() {
                        vars.entry(abs).and_modify(|v| v.has_dynamic = true);
                    }
                    var_eu_access.entry(abs).or_default().insert(eu_idx);
                }
                SIRInstruction::Store(addr, offset, _, _, _, _)
                    if addr.region == WORKING_REGION =>
                {
                    let abs = addr.absolute_addr();
                    if offset.is_dynamic() {
                        vars.entry(abs).and_modify(|v| v.has_dynamic = true);
                    }
                    var_eu_access.entry(abs).or_default().insert(eu_idx);
                }
                SIRInstruction::Store(addr, _, _, _, _, _)
                    if addr.region == SPARSE_WORKING_REGION =>
                {
                    let abs = addr.absolute_addr();
                    sparse_vars
                        .entry(abs)
                        .or_insert(SparseVarInfo {
                            stable_addr: None,
                            has_store: false,
                            invalid_apply: false,
                        })
                        .has_store = true;
                }
                _ => {}
            }
        }
    }

    // Phase 2: Determine eligible variables
    let eligible: std::collections::HashSet<AbsoluteAddr> = vars
        .iter()
        .filter(|(abs, info)| {
            // Must have at least one seed and one apply
            if info.seed_locs.is_empty() || info.apply_locs.is_empty() {
                return false;
            }
            // No dynamic offsets
            if info.has_dynamic {
                return false;
            }
            // Independence: only accessed by one original EU
            if !eu_boundary_blocks.is_empty() {
                if let Some(eus) = var_eu_access.get(*abs) {
                    if eus.len() > 1 {
                        return false;
                    }
                }
            }
            true
        })
        .map(|(abs, _)| *abs)
        .collect();

    let unsafe_after_store = super::commit_ops::direct_stable_store_hazards(eu);
    let eligible: std::collections::HashSet<AbsoluteAddr> = eligible
        .into_iter()
        .filter(|addr| !unsafe_after_store.contains_addr(*addr))
        .collect();

    // Sparse state has no per-EU seed: every producer writes the same pending
    // value in merged event order.  Therefore producer count is not a safety
    // condition; only an observation/competing write before the tail Commit is.
    let sparse_eligible: std::collections::HashSet<AbsoluteAddr> = sparse_vars
        .iter()
        .filter(|(addr, info)| {
            info.has_store
                && info.stable_addr.is_some()
                && !info.invalid_apply
                && !unsafe_after_store.contains_addr(**addr)
        })
        .map(|(addr, _)| *addr)
        .collect();

    if eligible.is_empty() && sparse_eligible.is_empty() {
        return;
    }

    // Build AbsoluteAddr → stable RegionedAbsoluteAddr mapping
    let stable_addrs: HashMap<AbsoluteAddr, RegionedAbsoluteAddr> = eligible
        .iter()
        .filter_map(|abs| vars.get(abs).map(|info| (*abs, info.stable_addr)))
        .collect();
    let sparse_stable_addrs: HashMap<AbsoluteAddr, RegionedAbsoluteAddr> = sparse_eligible
        .iter()
        .filter_map(|abs| {
            sparse_vars
                .get(abs)
                .and_then(|info| info.stable_addr.map(|stable| (*abs, stable)))
        })
        .collect();

    // Phase 3: Rewrite — redirect WORKING → STABLE, remove Commits
    for block in eu.blocks.values_mut() {
        block.instructions.retain_mut(|inst| {
            match inst {
                // Remove seed and apply Commits for eligible variables
                SIRInstruction::Commit(src, dst, _, _, _) => {
                    if src.region == STABLE_REGION && dst.region == WORKING_REGION {
                        let abs = src.absolute_addr();
                        if eligible.contains(&abs) {
                            return false;
                        } // remove seed
                    }
                    if src.region == WORKING_REGION && dst.region == STABLE_REGION {
                        let abs = src.absolute_addr();
                        if eligible.contains(&abs) {
                            return false;
                        } // remove apply
                    }
                    if src.region == SPARSE_WORKING_REGION && dst.region == STABLE_REGION {
                        let abs = src.absolute_addr();
                        if sparse_eligible.contains(&abs) {
                            return false;
                        }
                    }
                    true
                }
                // Redirect Load from WORKING to STABLE
                SIRInstruction::Load(_, addr, _, _) if addr.region == WORKING_REGION => {
                    let abs = addr.absolute_addr();
                    if let Some(stable) = stable_addrs.get(&abs) {
                        *addr = *stable;
                    }
                    true
                }
                // Redirect Store from WORKING to STABLE
                SIRInstruction::Store(addr, _, _, _, _, _) if addr.region == WORKING_REGION => {
                    let abs = addr.absolute_addr();
                    if let Some(stable) = stable_addrs.get(&abs) {
                        *addr = *stable;
                    }
                    true
                }
                // A proved sparse Store needs neither its lazy chunk copy nor
                // dirty metadata once it publishes directly to STABLE.
                SIRInstruction::Store(addr, _, _, _, _, _)
                    if addr.region == SPARSE_WORKING_REGION =>
                {
                    let abs = addr.absolute_addr();
                    if let Some(stable) = sparse_stable_addrs.get(&abs) {
                        *addr = *stable;
                    }
                    true
                }
                _ => true,
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HashMap;
    use veryl_analyzer::ir::VarId;

    fn addr(region: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region,
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(0),
        }
    }

    fn block(
        id: usize,
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        terminator: SIRTerminator,
    ) -> BasicBlock<RegionedAbsoluteAddr> {
        BasicBlock {
            id: BlockId(id),
            params: Vec::new(),
            instructions,
            terminator,
        }
    }

    fn sparse_store() -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Store(
            addr(SPARSE_WORKING_REGION),
            SIROffset::Element {
                index: RegisterId(0),
                element_width: 8,
                bit_offset: 0,
                dynamic_bit_offset: None,
            },
            8,
            RegisterId(1),
            Vec::new(),
            Vec::new(),
        )
    }

    fn sparse_commit() -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Commit(
            addr(SPARSE_WORKING_REGION),
            addr(STABLE_REGION),
            SIROffset::Static(0),
            64,
            Vec::new(),
        )
    }

    fn eu(blocks: Vec<BasicBlock<RegionedAbsoluteAddr>>) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut register_map = HashMap::default();
        register_map.insert(
            RegisterId(0),
            RegisterType::Bit {
                width: 3,
                signed: false,
            },
        );
        register_map.insert(RegisterId(1), RegisterType::Logic { width: 8 });
        register_map.insert(RegisterId(2), RegisterType::Logic { width: 8 });
        register_map.insert(
            RegisterId(3),
            RegisterType::Bit {
                width: 1,
                signed: false,
            },
        );
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: blocks.into_iter().map(|block| (block.id, block)).collect(),
            register_map,
        }
    }

    #[test]
    fn indexed_sparse_round_trip_is_redirected_when_the_event_has_no_hazard() {
        let mut unit = eu(vec![
            block(
                0,
                vec![
                    SIRInstruction::Load(
                        RegisterId(2),
                        addr(STABLE_REGION),
                        SIROffset::Static(0),
                        8,
                    ),
                    sparse_store(),
                ],
                SIRTerminator::Jump(BlockId(1), Vec::new()),
            ),
            block(1, vec![sparse_commit()], SIRTerminator::Return),
        ]);

        eliminate_working_round_trip(&mut unit, &[BlockId(1)]);

        assert!(matches!(
            unit.blocks[&BlockId(0)].instructions.as_slice(),
            [SIRInstruction::Load(..), SIRInstruction::Store(address, SIROffset::Element { .. }, ..)]
                if address.region == STABLE_REGION
        ));
        assert!(unit.blocks[&BlockId(1)].instructions.is_empty());
    }

    #[test]
    fn indexed_sparse_round_trip_stays_private_across_an_old_stable_read() {
        let mut unit = eu(vec![
            block(
                0,
                vec![sparse_store()],
                SIRTerminator::Jump(BlockId(1), Vec::new()),
            ),
            block(
                1,
                vec![
                    SIRInstruction::Load(
                        RegisterId(2),
                        addr(STABLE_REGION),
                        SIROffset::Static(0),
                        8,
                    ),
                    sparse_commit(),
                ],
                SIRTerminator::Return,
            ),
        ]);

        eliminate_working_round_trip(&mut unit, &[BlockId(1)]);

        assert!(matches!(
            unit.blocks[&BlockId(0)].instructions[0],
            SIRInstruction::Store(address, ..) if address.region == SPARSE_WORKING_REGION
        ));
        assert!(matches!(
            unit.blocks[&BlockId(1)].instructions[1],
            SIRInstruction::Commit(..)
        ));
    }

    #[test]
    fn sparse_round_trip_preserves_order_across_two_evaluators() {
        let mut unit = eu(vec![
            block(
                0,
                vec![sparse_store()],
                SIRTerminator::Jump(BlockId(1), Vec::new()),
            ),
            block(
                1,
                vec![sparse_store()],
                SIRTerminator::Jump(BlockId(2), Vec::new()),
            ),
            block(2, vec![sparse_commit()], SIRTerminator::Return),
        ]);

        eliminate_working_round_trip(&mut unit, &[BlockId(1), BlockId(2)]);

        assert!(matches!(
            unit.blocks[&BlockId(0)].instructions[0],
            SIRInstruction::Store(address, ..) if address.region == STABLE_REGION
        ));
        assert!(matches!(
            unit.blocks[&BlockId(1)].instructions[0],
            SIRInstruction::Store(address, ..) if address.region == STABLE_REGION
        ));
        assert!(unit.blocks[&BlockId(2)].instructions.is_empty());
    }

    #[test]
    fn sparse_round_trip_is_not_redirected_on_an_unpublished_exit_path() {
        let mut unit = eu(vec![
            block(
                0,
                Vec::new(),
                SIRTerminator::Branch {
                    cond: RegisterId(3),
                    true_block: (BlockId(1), Vec::new()),
                    false_block: (BlockId(2), Vec::new()),
                },
            ),
            block(1, vec![sparse_store()], SIRTerminator::Return),
            block(2, vec![sparse_commit()], SIRTerminator::Return),
        ]);

        eliminate_working_round_trip(&mut unit, &[BlockId(2)]);

        assert!(matches!(
            unit.blocks[&BlockId(1)].instructions[0],
            SIRInstruction::Store(address, ..) if address.region == SPARSE_WORKING_REGION
        ));
        assert!(matches!(
            unit.blocks[&BlockId(2)].instructions[0],
            SIRInstruction::Commit(..)
        ));
    }
}
