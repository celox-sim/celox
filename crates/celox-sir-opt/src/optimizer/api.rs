//! Public adapters for targeted SIR optimization operations.

use super::passes::control_flow::pass_guarded_region_sinking;
use super::passes::dataflow::pass_vectorize_concat;
use super::passes::memory::{
    fused_comb_dse, pass_dead_store_elimination, pass_global_store_load_forwarding,
    pass_identity_store_bypass,
};
use super::pipeline::pass_manager;
use super::*;

pub fn promote_eval_apply_working_round_trips(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
) -> bool {
    pass_global_store_load_forwarding::promote_eval_apply_working_round_trips(eu)
}

pub fn remove_dead_sir_definitions(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    pass_vectorize_concat::remove_dead_definitions(eu);
}

pub fn eliminate_unobserved_comb_state_stores(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    provenance: &crate::ir::SirMergeProvenance,
    first_ff_unit: usize,
) -> Result<usize, String> {
    fused_comb_dse::eliminate(eu, provenance, first_ff_unit)
}

pub fn eliminate_shared_comb_state_stores(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    direct_ff_writes: &[crate::ir::VarAtomBase<RegionedAbsoluteAddr>],
) -> Result<usize, String> {
    fused_comb_dse::eliminate_shared(eu, direct_ff_writes)
}

pub fn promote_fused_comb_static_slots(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
) -> Result<bool, String> {
    pass_global_store_load_forwarding::promote_fused_comb_static_slots(eu)
}

pub fn retain_final_identity_aliases(program: &mut OptimizationContext<'_>, four_state: bool) {
    pass_identity_store_bypass::retain_final_identity_aliases(program, four_state);
}

pub fn remove_final_identity_alias_stores(
    program: &mut OptimizationContext<'_>,
    validated_aliases: &crate::HashMap<AbsoluteAddr, AbsoluteAddr>,
    four_state: bool,
) {
    pass_identity_store_bypass::remove_final_identity_alias_stores(
        program,
        validated_aliases,
        four_state,
    );
}

pub fn optimize_rooted_comb_memory(
    program: &mut OptimizationContext<'_>,
    externally_live: &crate::HashSet<AbsoluteAddr>,
    four_state: bool,
) {
    pass_dead_store_elimination::eliminate_dead_stores(program, externally_live);
    let options = PassOptions {
        four_state,
        ..PassOptions::default()
    };
    for eu in &mut program.sir.eval_comb {
        pass_vectorize_concat::remove_dead_definitions(eu);
        pass_guarded_region_sinking::eliminate_dead_control_regions(eu);
        pass_manager::ExecutionUnitPass::run(&ControlFlowSimplifyPass, eu, &options);
        pass_vectorize_concat::remove_dead_definitions(eu);
        pass_guarded_region_sinking::sink_pure_values_with_predicate_repair(eu);
        pass_guarded_region_sinking::eliminate_dead_control_regions(eu);
        pass_manager::ExecutionUnitPass::run(&ControlFlowSimplifyPass, eu, &options);
        pass_vectorize_concat::remove_dead_definitions(eu);
    }
}
