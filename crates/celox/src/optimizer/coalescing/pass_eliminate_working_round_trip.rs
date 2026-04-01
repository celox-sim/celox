//! Eliminate working memory round-trip for independent FF patterns.
//!
//! Each eval_apply EU does: Seed(STABLE→WORKING) + compute + Store(WORKING) + Apply(WORKING→STABLE).
//! For independent FFs (variable accessed by only one EU), we redirect all WORKING accesses
//! to STABLE directly, eliminating both Commits (4 MIR instructions saved per variable).

use crate::ir::*;

/// Eliminate the WORKING memory round-trip for independent variables.
///
/// `eu_boundary_blocks`: block IDs that start a new original EU (after SIR merge).
/// Empty for pre-merge (single EU) — all variables are trivially independent.
#[cfg_attr(target_arch = "wasm32", allow(dead_code))]
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

    let mut vars: HashMap<AbsoluteAddr, VarInfo> = HashMap::new();

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
                SIRInstruction::Commit(src, dst, offset, _bits, _) => {
                    let abs = src.absolute_addr();
                    if src.region == STABLE_REGION && dst.region == WORKING_REGION {
                        // Seed: STABLE → WORKING
                        let has_dynamic = matches!(offset, SIROffset::Dynamic(_));
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
                        let has_dynamic = matches!(offset, SIROffset::Dynamic(_));
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
                    }
                }
                SIRInstruction::Load(_, addr, offset, _) if addr.region == WORKING_REGION => {
                    let abs = addr.absolute_addr();
                    if matches!(offset, SIROffset::Dynamic(_)) {
                        vars.entry(abs).and_modify(|v| v.has_dynamic = true);
                    }
                    var_eu_access.entry(abs).or_default().insert(eu_idx);
                }
                SIRInstruction::Store(addr, offset, _, _, _) if addr.region == WORKING_REGION => {
                    let abs = addr.absolute_addr();
                    if matches!(offset, SIROffset::Dynamic(_)) {
                        vars.entry(abs).and_modify(|v| v.has_dynamic = true);
                    }
                    var_eu_access.entry(abs).or_default().insert(eu_idx);
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

    if eligible.is_empty() {
        return;
    }

    // Build AbsoluteAddr → stable RegionedAbsoluteAddr mapping
    let stable_addrs: HashMap<AbsoluteAddr, RegionedAbsoluteAddr> = eligible
        .iter()
        .filter_map(|abs| vars.get(abs).map(|info| (*abs, info.stable_addr)))
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
                SIRInstruction::Store(addr, _, _, _, _) if addr.region == WORKING_REGION => {
                    let abs = addr.absolute_addr();
                    if let Some(stable) = stable_addrs.get(&abs) {
                        *addr = *stable;
                    }
                    true
                }
                _ => true,
            }
        });
    }
}
