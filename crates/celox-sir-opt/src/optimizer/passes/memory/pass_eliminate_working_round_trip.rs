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
pub fn eliminate_working_round_trip(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    _eu_boundary_blocks: &[BlockId],
) {
    eliminate_working_round_trip_impl(eu, false);
}

/// Optimize one partial event task. Pending values can have producers outside
/// this function, so publication requires proof of a local definition on every
/// path. Sparse publication can also consume dirty entries from other tasks.
pub fn eliminate_partial_working_round_trip(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    eliminate_working_round_trip_impl(eu, true);
}

fn eliminate_working_round_trip_impl(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, partial: bool) {
    use crate::HashMap;
    // An image-local callee can observe or publish any working slot. Its
    // contents are outside this function's proof of independent publication.
    if eu
        .blocks
        .values()
        .flat_map(|block| &block.instructions)
        .any(|instruction| {
            instruction.accesses_unknown_state()
                || instruction.call_effects().is_some_and(|effects| {
                    effects
                        .reads
                        .iter()
                        .chain(&effects.writes)
                        .any(|addr| addr.region != celox_design::STABLE_REGION)
                })
        })
    {
        return;
    }

    // Phase 1: Scan — collect info about each WORKING variable
    struct VarInfo {
        stable_addr: RegionedAbsoluteAddr,
        seed_locs: Vec<(BlockId, usize)>,  // Commit(STABLE→WORKING)
        apply_locs: Vec<(BlockId, usize)>, // Commit(WORKING→STABLE)
        has_dynamic_commit: bool,
    }

    struct SparseVarInfo {
        stable_addr: Option<RegionedAbsoluteAddr>,
        has_store: bool,
        invalid_apply: bool,
    }

    let mut vars: HashMap<AbsoluteAddr, VarInfo> = HashMap::default();
    let mut sparse_vars: HashMap<AbsoluteAddr, SparseVarInfo> = HashMap::default();
    let mut working_loads = crate::HashSet::default();
    let mut working_stores = crate::HashSet::default();

    for (&bid, block) in &eu.blocks {
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
                            has_dynamic_commit: false,
                        });
                        entry.seed_locs.push((bid, ii));
                        if has_dynamic {
                            entry.has_dynamic_commit = true;
                        }
                    } else if src.region == WORKING_REGION && dst.region == STABLE_REGION {
                        // Apply: WORKING → STABLE
                        let abs_w = src.absolute_addr();
                        let has_dynamic = offset.is_dynamic();
                        let entry = vars.entry(abs_w).or_insert_with(|| VarInfo {
                            stable_addr: *dst,
                            seed_locs: Vec::new(),
                            apply_locs: Vec::new(),
                            has_dynamic_commit: false,
                        });
                        entry.apply_locs.push((bid, ii));
                        if has_dynamic {
                            entry.has_dynamic_commit = true;
                        }
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
                SIRInstruction::Load(_, addr, _, _) if addr.region == WORKING_REGION => {
                    let abs = addr.absolute_addr();
                    working_loads.insert(abs);
                }
                SIRInstruction::Store(addr, _, _, _, _, _) if addr.region == WORKING_REGION => {
                    let abs = addr.absolute_addr();
                    working_stores.insert(abs);
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
    let eligible: crate::HashSet<AbsoluteAddr> = vars
        .iter()
        .filter(|(abs, info)| {
            // A seeded slot may retain its old value on paths without a
            // Store. If StateSSA already proved the seed dead, direct
            // publication is still valid when the slot is fully defined and
            // no operation reads the private WORKING value.
            if info.apply_locs.is_empty()
                || (info.seed_locs.is_empty()
                    && (!working_stores.contains(*abs) || working_loads.contains(*abs)))
            {
                return false;
            }
            // Removing a partial dynamically addressed seed/apply would change
            // which bytes are initialized or published.  Dynamic data accesses
            // themselves are safe: the CFG memory-dependence analysis below
            // uses a conservative alias envelope and rejects every path with
            // an intervening STABLE observation or competing write.
            if info.has_dynamic_commit {
                return false;
            }
            true
        })
        .map(|(abs, _)| *abs)
        .collect();

    let unsafe_after_store = super::commit_ops::direct_stable_store_hazards(eu);
    let uninitialized = (partial && !eligible.is_empty())
        .then(|| super::commit_ops::uninitialized_working_observations(eu));
    let eligible: crate::HashSet<AbsoluteAddr> = eligible
        .into_iter()
        .filter(|addr| {
            !unsafe_after_store.contains_addr(*addr)
                && !uninitialized
                    .as_ref()
                    .is_some_and(|undefined| undefined.contains_addr(*addr))
        })
        .collect();

    // Sparse state has no per-EU seed: every producer writes the same pending
    // value in merged event order.  Therefore producer count is not a safety
    // condition; only an observation/competing write before the tail Commit is.
    let sparse_eligible: crate::HashSet<AbsoluteAddr> = sparse_vars
        .iter()
        .filter(|(addr, info)| {
            !partial
                && info.has_store
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
mod tests;
