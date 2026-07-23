//! Construction of the pre-register-allocation MIR optimization pipeline.

use super::*;

struct PassRunner<'a> {
    func: &'a mut MFunction,
    verify_each: bool,
}

impl<'a> PassRunner<'a> {
    fn new(func: &'a mut MFunction) -> Self {
        Self {
            func,
            verify_each: std::env::var_os("CELOX_MIR_VERIFY_PASSES").is_some(),
        }
    }

    fn run(&mut self, name: &'static str, pass: impl FnOnce(&mut MFunction)) {
        pass(self.func);
        if self.verify_each
            && let Err(error) = self.func.verify_result()
        {
            panic!("after MIR pass {name}: {error}");
        }
    }

    fn has_high_pressure(&self) -> bool {
        self.func.vregs.count() > 40
    }

    fn has_bmi2(&self) -> bool {
        self.func.target_features.bmi2()
    }
}

/// Run all MIR optimization passes.
pub fn optimize(func: &mut MFunction) {
    let mut runner = PassRunner::new(func);
    if runner.has_high_pressure() {
        run_high_pressure_pipeline(&mut runner);
    } else {
        run_low_pressure_pipeline(&mut runner);
    }
    run_final_pipeline(&mut runner);

    if cfg!(debug_assertions) || std::env::var_os("CELOX_MIR_VERIFY").is_some() {
        if let Err(error) = runner.func.verify_result() {
            panic!("after MIR optimizer: {error}");
        }
    }
}

fn run_high_pressure_pipeline(runner: &mut PassRunner<'_>) {
    runner.run("fold_proven_comparisons", fold_proven_comparisons);
    for iteration in 0..2 {
        runner.run("constant_fold", constant_fold);
        runner.run("constant_dedup", constant_dedup);
        runner.run("copy_propagate", copy_propagate);
        runner.run(
            "promote_partial_store_round_trips",
            promote_partial_store_round_trips,
        );
        runner.run("forward_local_store_loads", forward_local_store_loads);
        runner.run(
            "eliminate_redundant_local_stores",
            eliminate_redundant_local_stores,
        );
        runner.run("algebraic_simplify", algebraic_simplify);
        runner.run("redundant_mask_eliminate", redundant_mask_eliminate);
        runner.run("fold_bit_toggle_insert", fold_bit_toggle_insert);
        // Expose target-rematerializable one-source operations to the final
        // GVN iteration. Keeping these as register-register forms until after
        // GVN made equal index calculations look like arbitrary two-input
        // expressions, so GVN deliberately recomputed them instead of leaving
        // the carry/rematerialize choice to allocation.
        if iteration == 1 {
            runner.run("pre_gvn_lower_to_imm_forms", lower_to_imm_forms);
        }
        runner.run("global_gvn", global_gvn);
        runner.run("dead_code_eliminate", dead_code_eliminate);
    }
    runner.run("fold_boolean_normalizations", fold_boolean_normalizations);
    runner.run("redundant_mask_eliminate", redundant_mask_eliminate);
    runner.run("copy_propagate", copy_propagate);
    runner.run("dead_code_eliminate", dead_code_eliminate);
    runner.run("fuse_compare_selects", fuse_compare_selects);
    runner.run("dead_code_eliminate", dead_code_eliminate);
    runner.run("sink_selected_indexed_loads", sink_selected_indexed_loads);
    runner.run("dead_code_eliminate", dead_code_eliminate);
    runner.run("sink_loads", sink_loads);
    runner.run("eliminate_redundant_or_terms", eliminate_redundant_or_terms);
    runner.run("fold_contiguous_load_packs", fold_contiguous_load_packs);
    runner.run("dead_code_eliminate", dead_code_eliminate);
    run_target_bit_folds(runner);
    runner.run("fold_add_chain_to_popcnt", fold_add_chain_to_popcnt);
    runner.run("dead_code_eliminate", dead_code_eliminate);
}

fn run_low_pressure_pipeline(runner: &mut PassRunner<'_>) {
    runner.run("fold_proven_comparisons", fold_proven_comparisons);
    runner.run("constant_fold", constant_fold);
    runner.run("constant_dedup", constant_dedup);
    runner.run("copy_propagate", copy_propagate);
    runner.run(
        "promote_partial_store_round_trips",
        promote_partial_store_round_trips,
    );
    runner.run("forward_local_store_loads", forward_local_store_loads);
    runner.run(
        "eliminate_redundant_local_stores",
        eliminate_redundant_local_stores,
    );
    runner.run("algebraic_simplify", algebraic_simplify);
    runner.run("redundant_mask_eliminate", redundant_mask_eliminate);
    runner.run("fold_bit_toggle_insert", fold_bit_toggle_insert);
    runner.run("eliminate_redundant_or_terms", eliminate_redundant_or_terms);
    runner.run("fold_contiguous_load_packs", fold_contiguous_load_packs);
    run_target_bit_folds(runner);
    runner.run("fold_add_chain_to_popcnt", fold_add_chain_to_popcnt);
    runner.run("dead_code_eliminate", dead_code_eliminate);
    runner.run("lower_to_imm_forms", lower_to_imm_forms);
    runner.run("fold_boolean_normalizations", fold_boolean_normalizations);
    runner.run("redundant_mask_eliminate", redundant_mask_eliminate);
    runner.run("copy_propagate", copy_propagate);
    runner.run("dead_code_eliminate", dead_code_eliminate);
    runner.run("fuse_compare_selects", fuse_compare_selects);
    runner.run("dead_code_eliminate", dead_code_eliminate);
    runner.run("sink_selected_indexed_loads", sink_selected_indexed_loads);
    runner.run("dead_code_eliminate", dead_code_eliminate);
}

fn run_target_bit_folds(runner: &mut PassRunner<'_>) {
    if runner.has_bmi2() {
        runner.run(
            "fold_byte_enable_spread_to_pdep",
            fold_byte_enable_spread_to_pdep,
        );
        runner.run("fold_deposit_chain_to_pdep", fold_deposit_chain_to_pdep);
        runner.run("fold_extract_chain_to_pext", fold_extract_chain_to_pext);
        runner.run("fold_xor_chain_to_pext", fold_xor_chain_to_pext);
    }
}

fn run_final_pipeline(runner: &mut PassRunner<'_>) {
    // Select fusion and late value numbering can make both arms identical,
    // while immediate lowering can expose a machine-width identity only after
    // the main algebraic iterations have finished. Close those transformations
    // before allocation: otherwise the dead predicate and identity result both
    // acquire live ranges, phi copies, and possible spill homes.
    runner.run(
        "final_simplify_equal_value_selects",
        simplify_equal_value_selects,
    );
    runner.run("final_algebraic_simplify", algebraic_simplify);
    runner.run("final_copy_propagate", copy_propagate);
    runner.run("final_constant_fold", constant_fold);
    runner.run("final_lower_to_imm_forms", lower_to_imm_forms);
    runner.run(
        "fold_reconstructed_bit_partitions",
        fold_reconstructed_bit_partitions,
    );
    runner.run("post_lower_algebraic_simplify", algebraic_simplify);
    runner.run("post_lower_copy_propagate", copy_propagate);
    runner.run(
        "fold_contiguous_memory_copies",
        fold_contiguous_memory_copies,
    );
    runner.run("fold_scaled_indexed_loads", fold_scaled_indexed_loads);
    runner.run("final_dead_code_eliminate", dead_code_eliminate);
    runner.run("simplify_cfg", simplify_cfg);
    // CFG simplification concatenates linear blocks. Re-place constants only
    // after that concatenation, otherwise a block-local constant can acquire a
    // very long artificial live range in the merged block.
    runner.run("final_sink_loads", sink_loads);
    runner.run("refresh_constant_spill_descs", refresh_constant_spill_descs);
}
