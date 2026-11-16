//! Parts, region copies and sparse commits of flip-flop logic.

use super::*;

/// Evaluate and apply units of one independently lowered FF part.
pub(super) fn build_ff_part(
    mut builder: SIRBuilder<RegionedVarAddr>,
    ff_group: &crate::lowering::ff::FfGroupParseResult,
) -> crate::symbolic::artifact::FfPart<RegionedVarAddr> {
    let targets = &ff_group.targets;
    let dynamic_write_vars = &ff_group.dynamic_write_vars;
    builder.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = builder.drain();
    let mut evaluate = ExecutionUnit {
        blocks,
        entry_block_id: BlockId(0),
        register_map,
    };
    rewrite_dynamic_ff_stores_to_sparse(&mut evaluate, dynamic_write_vars);
    let seeds =
        build_ff_region_copies_skipping(targets, STABLE_REGION, WORKING_REGION, dynamic_write_vars);
    if let Some(entry) = evaluate.blocks.get_mut(&BlockId(0)) {
        let mut instructions = seeds;
        instructions.append(&mut entry.instructions);
        entry.instructions = instructions;
    }
    let mut commits =
        build_ff_region_copies_skipping(targets, WORKING_REGION, STABLE_REGION, dynamic_write_vars);
    commits.extend(build_sparse_ff_commits(targets, dynamic_write_vars));
    let mut apply_builder = SIRBuilder::new();
    for commit in commits {
        apply_builder.emit(commit);
    }
    apply_builder.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = apply_builder.drain();
    crate::symbolic::artifact::FfPart {
        evaluate,
        apply: ExecutionUnit {
            blocks,
            entry_block_id: BlockId(0),
            register_map,
        },
    }
}

pub(super) fn build_ff_region_copies_skipping(
    targets: &[VarAtomBase<RegionedVarAddr>],
    src_region: u32,
    dst_region: u32,
    skip_vars: &HashSet<VarId>,
) -> Vec<SIRInstruction<RegionedVarAddr>> {
    let mut ranges_by_var: HashMap<VarId, Vec<BitAccess>> = HashMap::default();
    let mut var_order = Vec::new();
    let mut seen_vars = HashSet::default();
    for target in targets {
        if skip_vars.contains(&target.id.var_id) {
            continue;
        }
        if seen_vars.insert(target.id.var_id) {
            var_order.push(target.id.var_id);
        }
        ranges_by_var
            .entry(target.id.var_id)
            .or_default()
            .push(target.access);
    }

    let mut copies = Vec::new();
    for var_id in var_order {
        let Some(mut ranges) = ranges_by_var.remove(&var_id) else {
            continue;
        };
        ranges.sort_by_key(|range| (range.lsb, range.msb));

        let mut current: Option<BitAccess> = None;
        for range in ranges {
            match current {
                Some(mut cur) if range.lsb <= cur.msb.saturating_add(1) => {
                    cur.msb = cur.msb.max(range.msb);
                    current = Some(cur);
                }
                Some(cur) => {
                    push_ff_region_copy(&mut copies, var_id, cur, src_region, dst_region);
                    current = Some(range);
                }
                None => current = Some(range),
            }
        }
        if let Some(cur) = current {
            push_ff_region_copy(&mut copies, var_id, cur, src_region, dst_region);
        }
    }

    copies
}

pub(super) fn build_sparse_ff_commits(
    targets: &[VarAtomBase<RegionedVarAddr>],
    sparse_vars: &HashSet<VarId>,
) -> Vec<SIRInstruction<RegionedVarAddr>> {
    let mut widths = HashMap::<VarId, usize>::default();
    let mut order = Vec::new();
    for target in targets {
        if !sparse_vars.contains(&target.id.var_id) {
            continue;
        }
        if !widths.contains_key(&target.id.var_id) {
            order.push(target.id.var_id);
        }
        widths
            .entry(target.id.var_id)
            .and_modify(|width| *width = (*width).max(target.access.msb.saturating_add(1)))
            .or_insert_with(|| target.access.msb.saturating_add(1));
    }
    order
        .into_iter()
        .map(|var_id| {
            SIRInstruction::Commit(
                RegionedVarAddr {
                    region: SPARSE_WORKING_REGION,
                    var_id,
                },
                RegionedVarAddr {
                    region: STABLE_REGION,
                    var_id,
                },
                SIROffset::Static(0),
                widths[&var_id],
                Vec::new(),
            )
        })
        .collect()
}

pub(super) fn rewrite_dynamic_ff_stores_to_sparse(
    eu: &mut ExecutionUnit<RegionedVarAddr>,
    dynamic_write_vars: &HashSet<VarId>,
) {
    if dynamic_write_vars.is_empty() {
        return;
    }
    for block in eu.blocks.values_mut() {
        for inst in &mut block.instructions {
            if let SIRInstruction::Store(addr, _, _, _, _, _) = inst
                && addr.region == WORKING_REGION
                && dynamic_write_vars.contains(&addr.var_id)
            {
                addr.region = SPARSE_WORKING_REGION;
            }
        }
    }
}

pub(super) fn push_ff_region_copy(
    copies: &mut Vec<SIRInstruction<RegionedVarAddr>>,
    var_id: VarId,
    range: BitAccess,
    src_region: u32,
    dst_region: u32,
) {
    copies.push(SIRInstruction::Commit(
        RegionedVarAddr {
            region: src_region,
            var_id,
        },
        RegionedVarAddr {
            region: dst_region,
            var_id,
        },
        SIROffset::Static(range.lsb),
        range.msb - range.lsb + 1,
        Vec::new(),
    ));
}

pub(super) fn remap_for_effect_site_ids<A: std::hash::Hash + Eq + Clone>(
    arena: &mut SLTNodeArena<A>,
    range: std::ops::Range<usize>,
    offset: u32,
) -> Result<(), ParserError> {
    if offset == 0 {
        return Ok(());
    }
    arena
        .remap_for_fold_effect_sites(range, |site_id, fatal_error_code| {
            site_id
                .checked_add(offset)
                .map(|site_id| Some((site_id, fatal_error_code)))
                .ok_or(SLTNodeArenaEditError::SiteIdOverflow { site_id, offset })
        })
        .map_err(|error| {
            ParserError::illegal_context("ForFold runtime-event remap", error.to_string(), None)
        })
}

pub(super) fn collect_glue_sources(
    expr: NodeId,
    arena: &SLTNodeArena<GlueAddr>,
) -> HashSet<VarAtomBase<GlueAddr>> {
    let mut set = HashSet::default();
    collect_glue_sources_with_window(expr, None, arena, &mut set);
    set
}
