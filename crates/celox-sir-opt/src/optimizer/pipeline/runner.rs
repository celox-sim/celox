//! Construction and execution of the whole-program SIR pass pipeline.

use super::cache::optimize_unit_groups_cached;
use super::*;
use crate::optimizer::passes::memory::{pass_commit_sinking, pass_inline_commit_forwarding};

fn optimize_unified_commit_groups(
    groups: &mut crate::HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    sink: bool,
    forward: bool,
) {
    if !sink && !forward {
        return;
    }
    for units in groups.values_mut() {
        if units.is_empty() {
            continue;
        }
        // Hazard analysis must see the complete event order, not an individual
        // always_ff/module EU.  The actual rewrites remain local to each EU.
        let hazards = if units.len() == 1 {
            commit_ops::direct_stable_store_hazards(&units[0])
        } else {
            let (merged, _) = crate::ir::merge_sir_eus(units);
            commit_ops::direct_stable_store_hazards(&merged)
        };
        if sink {
            pass_commit_sinking::run_complete_event(units, &hazards);
        }
        if forward {
            pass_inline_commit_forwarding::run_complete_event(units, &hazards);
        }
    }
}

fn move_sparse_commits_to_event_tail(
    groups: &mut crate::HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
) {
    for units in groups.values_mut() {
        if units.len() <= 1 {
            continue;
        }
        let mut commits = Vec::new();
        for unit in units.iter_mut() {
            for block in unit.blocks.values_mut() {
                block.instructions.retain(|inst| {
                    if matches!(
                        inst,
                        SIRInstruction::Commit(src, dst, ..)
                            if src.region == SPARSE_WORKING_REGION
                                && dst.region == STABLE_REGION
                    ) {
                        commits.push(inst.clone());
                        false
                    } else {
                        true
                    }
                });
            }
        }
        if commits.is_empty() {
            continue;
        }
        let mut blocks = crate::HashMap::default();
        blocks.insert(
            BlockId(0),
            BasicBlock {
                id: BlockId(0),
                params: Vec::new(),
                instructions: commits,
                terminator: SIRTerminator::Return,
            },
        );
        units.push(ExecutionUnit {
            blocks,
            entry_block_id: BlockId(0),
            register_map: crate::HashMap::default(),
        });
    }
}

pub(super) fn canonicalize_required(program: &mut OptimizationContext<'_>) {
    // Sparse next-state data must stay invisible until every evaluator for the
    // event has sampled STABLE. Keep the commit in a final EU even at O0,
    // where all optional SIR transforms are disabled.
    move_sparse_commits_to_event_tail(&mut program.sir.eval_apply_ffs);
    move_sparse_commits_to_event_tail(&mut program.sir.eval_comb_apply_ffs);
}

pub(super) fn optimize_with_options(
    program: &mut OptimizationContext,
    max_inflight_loads: usize,
    four_state: bool,
    opt: &crate::OptimizeOptions,
    preserve_element_storage_layout: bool,
) {
    let timing = opt.diagnostics.pass_timing;
    let options = PassOptions {
        max_inflight_loads,
        four_state,
        optimize_options: opt.clone(),
        preserve_element_storage_layout,
    };
    let unpacked_element_widths = Arc::new(
        program
            .design
            .state_objects
            .iter()
            .filter_map(|(&address, metadata)| {
                let element_count = metadata
                    .array_dims
                    .iter()
                    .try_fold(1usize, |count, &dimension| count.checked_mul(dimension))?;
                (!metadata.array_dims.is_empty()
                    && element_count != 0
                    && metadata.width % element_count == 0)
                    .then_some((address, metadata.width / element_count))
            })
            .collect::<crate::HashMap<_, _>>(),
    );
    // A plain Static access names one semantic unpacked element regardless of
    // the physical layout eventually selected by the backend. Cross-element
    // aggregation must use an explicit PackedElements/array-copy operation.
    let element_widths = Arc::new(
        unpacked_element_widths
            .iter()
            .flat_map(|(&address, &element_width)| {
                [STABLE_REGION, WORKING_REGION, SPARSE_WORKING_REGION].map(move |region| {
                    (
                        RegionedAbsoluteAddr::from_absolute_addr(region, address),
                        element_width,
                    )
                })
            })
            .collect::<crate::HashMap<_, _>>(),
    );

    // Helper closure to check pass enablement.
    let on = |pass: SirPass| opt.is_enabled(pass);

    canonicalize_required(program);

    // 1. Unified Case (Fast Path): Full optimizations are safe.
    let phase_start = timing.then(crate::timing::now);
    if timing {
        for (trigger, units) in &program.sir.eval_apply_ffs {
            for (index, eu) in units.iter().enumerate() {
                let instruction_count: usize = eu
                    .blocks
                    .values()
                    .map(|block| block.instructions.len())
                    .sum();
                tracing::debug!(
                    "[phase] eval_apply_ffs trigger={trigger} eu[{index}]: blocks={} insts={} regs={}",
                    eu.blocks.len(),
                    instruction_count,
                    eu.register_map.len()
                );
            }
        }
    }
    // Each generated execution path has different legality constraints. The
    // named builders make those differences reviewable without interleaving
    // them with collection traversal and timing.
    let pipeline_builder = builder::PipelineBuilder::new(
        opt,
        Arc::clone(&unpacked_element_widths),
        Arc::clone(&element_widths),
    );
    let ff_passes = pipeline_builder.fused_ff(program);
    let comb_ff_passes = pipeline_builder.fused_comb_ff(program);
    let comb_ff_late_passes = pipeline_builder.fused_comb_ff_late(program);
    let ff_post_passes = pipeline_builder.fused_ff_post();
    let comb_passes = pipeline_builder.combinational(program);

    let ff_eu_count: usize = program.sir.eval_apply_ffs.values().map(Vec::len).sum();
    let comb_ff_eu_count: usize = program.sir.eval_comb_apply_ffs.values().map(Vec::len).sum();
    let eu_count = ff_eu_count + comb_ff_eu_count;
    let comb_phase_start = timing.then(crate::timing::now);
    let comb_eu_count = program.sir.eval_comb.len();
    let sir = &mut *program.sir;
    #[cfg(target_arch = "wasm32")]
    {
        for (i, eu) in sir.eval_comb.iter_mut().enumerate() {
            if timing {
                let inst_count: usize = eu.blocks.values().map(|b| b.instructions.len()).sum();
                let block_count = eu.blocks.len();
                tracing::debug!(
                    "[phase] eval_comb eu[{i}]: blocks={block_count} insts={inst_count}"
                );
            }
            comb_passes.run(eu, &options);
        }

        optimize_unit_groups_cached(&mut sir.eval_apply_ffs, &ff_passes, &options);
        optimize_unit_groups_cached(&mut sir.eval_comb_apply_ffs, &comb_ff_passes, &options);
        optimize_unit_groups_cached(&mut sir.eval_comb_apply_ffs, &comb_ff_late_passes, &options);
        optimize_unified_commit_groups(
            &mut sir.eval_apply_ffs,
            on(SirPass::CommitSinking),
            on(SirPass::InlineCommitForwarding),
        );
        optimize_unified_commit_groups(
            &mut sir.eval_comb_apply_ffs,
            on(SirPass::CommitSinking),
            on(SirPass::InlineCommitForwarding),
        );
        optimize_unit_groups_cached(&mut sir.eval_apply_ffs, &ff_post_passes, &options);
        optimize_unit_groups_cached(&mut sir.eval_comb_apply_ffs, &ff_post_passes, &options);
    }
    #[cfg(not(target_arch = "wasm32"))]
    std::thread::scope(|scope| {
        let comb_worker = scope.spawn(|| {
            for (i, eu) in sir.eval_comb.iter_mut().enumerate() {
                if timing {
                    let inst_count: usize = eu.blocks.values().map(|b| b.instructions.len()).sum();
                    let block_count = eu.blocks.len();
                    tracing::debug!(
                        "[phase] eval_comb eu[{i}]: blocks={block_count} insts={inst_count}"
                    );
                }
                comb_passes.run(eu, &options);
            }
        });

        optimize_unit_groups_cached(&mut sir.eval_apply_ffs, &ff_passes, &options);
        optimize_unit_groups_cached(&mut sir.eval_comb_apply_ffs, &comb_ff_passes, &options);

        // The late comb pipeline must start from the CFG produced by the complete
        // initial pipeline. Keep it in the same exact-equivalence cache: clock
        // and reset triggers commonly share the complete fused body.
        optimize_unit_groups_cached(&mut sir.eval_comb_apply_ffs, &comb_ff_late_passes, &options);

        optimize_unified_commit_groups(
            &mut sir.eval_apply_ffs,
            on(SirPass::CommitSinking),
            on(SirPass::InlineCommitForwarding),
        );
        optimize_unified_commit_groups(
            &mut sir.eval_comb_apply_ffs,
            on(SirPass::CommitSinking),
            on(SirPass::InlineCommitForwarding),
        );
        optimize_unit_groups_cached(&mut sir.eval_apply_ffs, &ff_post_passes, &options);
        optimize_unit_groups_cached(&mut sir.eval_comb_apply_ffs, &ff_post_passes, &options);

        comb_worker
            .join()
            .expect("combinational SIR optimization worker must not panic");
    });
    if let Some(s) = phase_start {
        tracing::debug!("[phase] eval_apply_ffs ({eu_count} EUs): {:?}", s.elapsed());
    }
    if let Some(s) = comb_phase_start {
        tracing::debug!("[phase] eval_comb ({comb_eu_count} EUs): {:?}", s.elapsed());
    }

    // 2. Logic-Only Cache (Split Path Phase 1):
    // MUST NOT use EliminateDeadWorkingStoresPass because the Commits are in Phase 2.
    let phase_start = timing.then(crate::timing::now);
    let eval_only_passes = pipeline_builder.eval_only(program);

    let eu_count: usize = program.sir.eval_only_ffs.values().map(|v| v.len()).sum();
    optimize_unit_groups_cached(&mut program.sir.eval_only_ffs, &eval_only_passes, &options);
    if let Some(s) = phase_start {
        tracing::debug!("[phase] eval_only_ffs ({eu_count} EUs): {:?}", s.elapsed());
    }

    // 3. Commit-Only Cache (Split Path Phase 2):
    let phase_start = timing.then(crate::timing::now);
    let apply_passes = pipeline_builder.apply_only();

    let eu_count: usize = program.sir.apply_ffs.values().map(|v| v.len()).sum();
    for units in program.sir.apply_ffs.values_mut() {
        for eu in units {
            apply_passes.run(eu, &options);
        }
    }
    if let Some(s) = phase_start {
        tracing::debug!("[phase] apply_ffs ({eu_count} EUs): {:?}", s.elapsed());
    }

    super::late::optimize_late_comb(program, opt, &options, &unpacked_element_widths);
    if opt.diagnostics.mux_chain_stats {
        super::diagnostics::dump_mux_chain_stats(&program.sir.eval_comb);
    }
}
