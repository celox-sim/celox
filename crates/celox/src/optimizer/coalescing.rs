use crate::ir::*;
use crate::optimizer::{PassOptions, ProgramPass, SirPass};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

mod block_opt;
pub(crate) mod commit_ops;
#[cfg(target_arch = "x86_64")]
mod control_region_feasibility;
pub mod cost_model;
mod dead_working_stores;
mod fused_comb_dse;
#[cfg(target_arch = "x86_64")]
mod lane_aggregate_feasibility;
mod pass_bit_extract_peephole;
mod pass_branchify_mux;
mod pass_circular_priority;
mod pass_coalesce_stores;
mod pass_commit_sinking;
mod pass_concat_folding;
mod pass_control_flow_simplify;
mod pass_dead_code_elimination;
pub(crate) mod pass_dead_store_elimination;
#[cfg(target_arch = "x86_64")]
mod pass_effect_case_dispatch;
mod pass_eliminate_dead_working_stores;
pub(crate) mod pass_eliminate_working_round_trip;
#[cfg(target_arch = "x86_64")]
mod pass_global_store_load_forwarding;
mod pass_guarded_region_sinking;
mod pass_gvn;
mod pass_hoist_common_branch_loads;
mod pass_identity_store_bypass;
mod pass_indexed_store_recovery;
mod pass_inline_commit_forwarding;
mod pass_loop_idiom;
mod pass_manager;
mod pass_masked_array_any;
mod pass_optimize_blocks;
mod pass_packed_scatter_store;
mod pass_partial_forward;
mod pass_phi_outcome_compression;
mod pass_reschedule;
mod pass_sparse_case_dispatch;
mod pass_split_coalesced_stores;
mod pass_split_wide_commits;
mod pass_store_load_forwarding;
pub(crate) mod pass_tail_call_split;
mod pass_vectorize_concat;
mod pass_xor_chain_folding;
mod placement_analysis;
mod shared;
mod sir_analysis;
mod state_ssa;

pub use pass_tail_call_split::TailCallChunk;

/// Keep explicit scalar stores only for small register-like arrays. Large
/// arrays must remain free to coalesce reset/initialization runs before the
/// ordinary SIR passes; their dynamic element accesses can still use a
/// strided native layout when the resulting whole Store is a bulk zero fill.
fn preserve_native_element_boundaries(
    array: &crate::backend::memory_layout::UnpackedArrayLayout,
) -> bool {
    (1..=64).contains(&array.element_width)
        && array.element_stride * 8 != array.element_width
        && array.plane_size <= 256
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn promote_eval_apply_working_round_trips(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
) -> bool {
    pass_global_store_load_forwarding::promote_eval_apply_working_round_trips(eu)
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn remove_dead_sir_definitions(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    pass_vectorize_concat::remove_dead_definitions(eu);
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn eliminate_unobserved_comb_state_stores(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    provenance: &crate::ir::SirMergeProvenance,
    first_ff_unit: usize,
) -> Result<usize, String> {
    fused_comb_dse::eliminate(eu, provenance, first_ff_unit)
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn eliminate_shared_comb_state_stores(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
) -> Result<usize, String> {
    fused_comb_dse::eliminate_shared(eu)
}

pub(crate) fn retain_final_identity_aliases(program: &mut Program, four_state: bool) {
    pass_identity_store_bypass::retain_final_identity_aliases(program, four_state);
}

pub(crate) fn remove_final_identity_alias_stores(
    program: &mut Program,
    validated_aliases: &crate::HashMap<AbsoluteAddr, AbsoluteAddr>,
    four_state: bool,
) {
    pass_identity_store_bypass::remove_final_identity_alias_stores(
        program,
        validated_aliases,
        four_state,
    );
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn analyze_lane_aggregate_feasibility(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &crate::backend::MemoryLayout,
    four_state: bool,
) -> Result<lane_aggregate_feasibility::LaneAggregateFeasibilityReport, String> {
    lane_aggregate_feasibility::analyze(eu, layout, four_state)
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn vectorize_around_lane_aggregate_plan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: &crate::lane_aggregate_plan::LaneAggregatePlan,
) -> Result<(), crate::ir::verify::SirVerifyError> {
    // The executable aggregate plan names exact SIR definitions and root
    // instruction sites. Keep only those definitions unchanged while
    // restoring ordinary Concat vectorization around them, including in the
    // same block.
    let mut protected_definitions = plan
        .nodes
        .iter()
        .flat_map(|node| node.lanes.iter().copied())
        .collect::<crate::HashSet<_>>();
    protected_definitions.extend(plan.dead_scalar_registers.iter().copied());
    protected_definitions.extend(plan.roots.iter().map(|root| root.original_root));
    for root_index in 0..plan.roots.len() {
        if let Some(inputs) = plan.scalar_inputs_for_root(root_index) {
            protected_definitions.extend(inputs);
        }
    }

    VectorizeConcatPass::after_lane_planning(Arc::default(), protected_definitions)
        .run(eu, &PassOptions::default());
    eu.verify_result()
}

#[cfg(target_arch = "x86_64")]
pub(crate) fn optimize_native_merged_chain(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &crate::backend::MemoryLayout,
    four_state: bool,
) -> Result<(), (&'static str, crate::ir::verify::SirVerifyError)> {
    let mut changed = false;
    if crate::ir::inline_single_predecessor_jumps(eu)
        .map_err(|error| ("during native jump inlining", error))?
    {
        changed = true;
    }
    eu.verify_result()
        .map_err(|error| ("after native jump inlining", error))?;
    let element_widths = Arc::new(
        layout
            .unpacked_arrays
            .iter()
            .filter(|(_, array)| preserve_native_element_boundaries(array))
            .flat_map(|(&address, array)| {
                [STABLE_REGION, WORKING_REGION, SPARSE_WORKING_REGION].map(move |region| {
                    (
                        RegionedAbsoluteAddr::from_absolute_addr(region, address),
                        array.element_width,
                    )
                })
            })
            .collect::<crate::HashMap<_, _>>(),
    );
    OptimizeBlocksPass {
        skip_final_schedule: false,
        element_widths: Arc::clone(&element_widths),
    }
    .run(eu, &PassOptions::default());
    eu.verify_result()
        .map_err(|error| ("after native block optimization", error))?;
    // The native function is assembled after the ordinary per-EU pipeline.
    // Merging exposes constants and control-flow facts across the old EU
    // boundaries, and OptimizeBlocks can expose more of them while rewriting
    // the merged blocks.  Run full SCCP here before preserving packed sinks:
    // otherwise constant branches and their entire dead scalar arms survive
    // into lane planning and native lowering.
    ControlFlowSimplifyPass.run(
        eu,
        &PassOptions {
            four_state,
            ..PassOptions::default()
        },
    );
    eu.verify_result()
        .map_err(|error| ("after native merged-chain CFG simplification", error))?;
    if !four_state && pass_vectorize_concat::expose_packed_bit_store_sinks(eu) {
        changed = true;
        // Lane-aggregate analysis consumes the exact packed publication shape
        // produced above.  Ordinary Concat vectorization intentionally erases
        // that shape, so defer it when the verified lane recipe is requested.
        // The aggregate lowering replaces the same scalar definitions and
        // publication sites; when no executable recipe is found, ISel keeps
        // the untouched scalar SIR as its semantic fallback.
        let preserve_lane_publications = std::env::var_os("CELOX_LANE_AGGREGATE_FEASIBILITY")
            .is_some()
            || crate::backend::native::lane_aggregate_codegen_enabled();
        if !preserve_lane_publications {
            VectorizeConcatPass::default().run(eu, &PassOptions::default());
            GvnPass.run(eu, &PassOptions::default());
        }
        eu.verify_result()
            .map_err(|error| ("after native packed bit-store vectorization", error))?;
    }
    let recovered_bit_maps = if four_state {
        0
    } else {
        pass_circular_priority::recover_native_fixed_bit_map_loops(eu)
    };
    if recovered_bit_maps != 0 {
        changed = true;
        GvnPass.run(eu, &PassOptions::default());
        OptimizeBlocksPass {
            skip_final_schedule: false,
            element_widths,
        }
        .run(eu, &PassOptions::default());
        VectorizeConcatPass::default().run(eu, &PassOptions::default());
        GvnPass.run(eu, &PassOptions::default());
        eu.verify_result()
            .map_err(|error| ("after native fixed bit-map recovery", error))?;
    }
    if !four_state
        && std::env::var_os("CELOX_EFFECT_CASE_DISPATCH").is_some()
        && let Some(result) = pass_effect_case_dispatch::run(eu)
    {
        changed = true;
        let dead_control_start = crate::timing::now();
        pass_guarded_region_sinking::eliminate_dead_control_regions(eu);
        let dead_control_ns = dead_control_start.elapsed().as_nanos();
        let cfg_cleanup_start = crate::timing::now();
        ControlFlowSimplifyPass.run(
            eu,
            &PassOptions {
                four_state,
                ..PassOptions::default()
            },
        );
        let cfg_cleanup_ns = cfg_cleanup_start.elapsed().as_nanos();
        eprintln!(
            "[effect-case-dispatch] origin=b{} selector=r{} cases={} sinks={} \
             path_local_exits={} estimated_saving={} planning_ms={:.3} rewrite_ms={:.3} \
             dead_control_ms={:.3} cfg_cleanup_ms={:.3}",
            result.origin.0,
            result.selector.0,
            result.explicit_cases,
            result.sinks,
            result.path_local_exits,
            result.estimated_saving,
            result.planning_ns as f64 / 1_000_000.0,
            result.rewrite_ns as f64 / 1_000_000.0,
            dead_control_ns as f64 / 1_000_000.0,
            cfg_cleanup_ns as f64 / 1_000_000.0,
        );
        eu.verify_result()
            .map_err(|error| ("after native effect-case dispatch", error))?;
    }
    if changed {
        pass_vectorize_concat::remove_dead_definitions(eu);
        eu.verify_result()
            .map_err(|error| ("after native merged-chain DCE", error))?;
    }
    Ok(())
}

pub(crate) fn optimize_rooted_comb_memory(
    program: &mut Program,
    externally_live: &crate::HashSet<AbsoluteAddr>,
    four_state: bool,
    enable_tail_split: bool,
) {
    pass_dead_store_elimination::eliminate_dead_stores(program, externally_live);
    let options = PassOptions {
        four_state,
        ..PassOptions::default()
    };
    for eu in &mut program.eval_comb {
        pass_vectorize_concat::remove_dead_definitions(eu);
        pass_guarded_region_sinking::eliminate_dead_control_regions(eu);
        pass_manager::ExecutionUnitPass::run(&ControlFlowSimplifyPass, eu, &options);
        pass_vectorize_concat::remove_dead_definitions(eu);
        pass_guarded_region_sinking::sink_pure_values_with_predicate_repair(eu);
        pass_guarded_region_sinking::eliminate_dead_control_regions(eu);
        pass_manager::ExecutionUnitPass::run(&ControlFlowSimplifyPass, eu, &options);
        pass_vectorize_concat::remove_dead_definitions(eu);
    }

    // The old plan refers to the pre-DSE EUs. Rebuild it from the
    // transformed function instead of compiling stale chunks.
    program.eval_comb_plan = None;
    if enable_tail_split {
        if let Some(chunks) = pass_tail_call_split::split_if_needed(&program.eval_comb, four_state)
        {
            program.eval_comb_plan = Some(EvalCombPlan::TailCallChunks(chunks));
        } else if let Some(plan) =
            pass_tail_call_split::split_if_needed_spilled(&program.eval_comb, four_state)
        {
            program.eval_comb_plan = Some(EvalCombPlan::MemorySpilled(plan));
        }
    }
}

use pass_bit_extract_peephole::BitExtractPeepholePass;
use pass_branchify_mux::BranchifyMuxPass;
use pass_circular_priority::CircularPriorityPass;
use pass_coalesce_stores::CoalesceStoresPass;
use pass_commit_sinking::CommitSinkingPass;
use pass_concat_folding::ConcatFoldingPass;
use pass_control_flow_simplify::{ControlFlowSimplifyPass, PostGvnCfgCleanupPass};
use pass_dead_code_elimination::DeadCodeEliminationPass;
use pass_eliminate_dead_working_stores::EliminateDeadWorkingStoresPass;
use pass_guarded_region_sinking::GuardedRegionSinkingPass;
use pass_gvn::GvnPass;
use pass_hoist_common_branch_loads::HoistCommonBranchLoadsPass;
use pass_indexed_store_recovery::IndexedStoreRecoveryPass;
use pass_loop_idiom::LoopIdiomPass;
use pass_manager::{ExecutionUnitPass, ExecutionUnitPassManager};
use pass_masked_array_any::MaskedArrayAnyPass;
use pass_optimize_blocks::OptimizeBlocksPass;
use pass_packed_scatter_store::PackedScatterStorePass;
use pass_partial_forward::PartialForwardPass;
use pass_phi_outcome_compression::PhiOutcomeCompressionPass;
use pass_reschedule::ReschedulePass;
use pass_sparse_case_dispatch::SparseCaseDispatchPass;
use pass_split_coalesced_stores::SplitCoalescedStoresPass;
use pass_split_wide_commits::SplitWideCommitsPass;
use pass_store_load_forwarding::StoreLoadForwardingPass;
use pass_vectorize_concat::VectorizeConcatPass;
use pass_xor_chain_folding::XorChainFoldingPass;

pub struct CoalescingPass;

impl ProgramPass for CoalescingPass {
    fn name(&self) -> &'static str {
        "coalescing"
    }

    fn run(&self, program: &mut Program, options: &PassOptions) {
        optimize_with_options(
            program,
            options.max_inflight_loads,
            options.four_state,
            &options.optimize_options,
            options.preserve_element_storage_layout,
        );
    }
}

struct FusedCombDsePass;

impl ExecutionUnitPass for FusedCombDsePass {
    fn name(&self) -> &'static str {
        "fused_comb_dse"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        if fused_comb_dse::eliminate_shared(eu).is_ok() {
            pass_vectorize_concat::remove_dead_definitions(eu);
        }
    }
}

fn optimize_unit_groups_cached(
    groups: &mut crate::HashMap<AbsoluteAddr, Vec<ExecutionUnit<RegionedAbsoluteAddr>>>,
    passes: &ExecutionUnitPassManager,
    options: &PassOptions,
) {
    let timing = std::env::var_os("CELOX_PASS_TIMING").is_some();
    let total_start = timing.then(crate::timing::now);
    #[derive(Debug, Clone, PartialEq, Eq, Hash)]
    struct UnitShape {
        entry: BlockId,
        blocks: usize,
        registers: usize,
        instructions: usize,
    }

    fn shape(units: &[ExecutionUnit<RegionedAbsoluteAddr>]) -> Vec<UnitShape> {
        units
            .iter()
            .map(|unit| UnitShape {
                entry: unit.entry_block_id,
                blocks: unit.blocks.len(),
                registers: unit.register_map.len(),
                instructions: unit
                    .blocks
                    .values()
                    .map(|block| block.instructions.len())
                    .sum(),
            })
            .collect()
    }

    fn fingerprint(units: &[ExecutionUnit<RegionedAbsoluteAddr>]) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        units.len().hash(&mut hasher);
        for unit in units {
            unit.entry_block_id.hash(&mut hasher);

            let mut block_ids = unit.blocks.keys().copied().collect::<Vec<_>>();
            block_ids.sort_unstable();
            block_ids.len().hash(&mut hasher);
            for block_id in block_ids {
                let block = &unit.blocks[&block_id];
                block_id.hash(&mut hasher);
                block.params.hash(&mut hasher);
                block.instructions.hash(&mut hasher);
                block.terminator.hash(&mut hasher);
            }

            let mut registers = unit.register_map.iter().collect::<Vec<_>>();
            registers.sort_unstable_by_key(|(register, _)| **register);
            registers.len().hash(&mut hasher);
            for (register, ty) in registers {
                register.hash(&mut hasher);
                ty.hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    struct EquivalenceClass {
        representative: AbsoluteAddr,
        aliases: Vec<AbsoluteAddr>,
        shape: Vec<UnitShape>,
        fingerprint: u64,
    }

    // Establish exact source equivalence before mutating any representative.
    // Keeping a cloned pre-optimization group in the old cache doubled the
    // live SIR and copied every unique group even when it had no alias.
    let mut addresses = groups.keys().copied().collect::<Vec<_>>();
    addresses.sort_unstable();
    let mut classes: Vec<EquivalenceClass> = Vec::new();
    for address in addresses {
        let candidate_shape = shape(&groups[&address]);
        let candidate_fingerprint = fingerprint(&groups[&address]);
        if let Some(class) = classes.iter_mut().find(|class| {
            class.shape == candidate_shape
                && class.fingerprint == candidate_fingerprint
                && groups[&class.representative] == groups[&address]
        }) {
            class.aliases.push(address);
        } else {
            classes.push(EquivalenceClass {
                representative: address,
                aliases: Vec::new(),
                shape: candidate_shape,
                fingerprint: candidate_fingerprint,
            });
        }
    }
    let aliases = classes
        .iter()
        .map(|class| class.aliases.len())
        .sum::<usize>();
    if let Some(start) = total_start {
        eprintln!(
            "[group-cache-timing] classify groups={} classes={} aliases={} elapsed={:?}",
            groups.len(),
            classes.len(),
            aliases,
            start.elapsed()
        );
    }

    for class in classes {
        {
            let units = groups
                .get_mut(&class.representative)
                .expect("equivalence-class representative must exist");
            for eu in units {
                passes.run(eu, options);
            }
        }
        if class.aliases.is_empty() {
            continue;
        }
        let optimized = groups[&class.representative].clone();
        for alias in class.aliases {
            *groups
                .get_mut(&alias)
                .expect("equivalence-class alias must exist") = optimized.clone();
        }
    }
    if let Some(start) = total_start {
        eprintln!(
            "[group-cache-timing] total groups={} elapsed={:?}",
            groups.len(),
            start.elapsed()
        );
    }
}

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

fn dump_mux_chain_stats(units: &[ExecutionUnit<RegionedAbsoluteAddr>]) {
    let mut rows = Vec::new();

    for (eu_idx, eu) in units.iter().enumerate() {
        for block in eu.blocks.values() {
            let mut defs: crate::HashMap<RegisterId, usize> = crate::HashMap::default();
            for (idx, inst) in block.instructions.iter().enumerate() {
                if let Some(dst) = shared::def_reg(inst) {
                    defs.insert(dst, idx);
                }
            }

            let mut mux_else_children = crate::HashSet::default();
            for inst in &block.instructions {
                if let SIRInstruction::Mux(_, _, _, else_val) = inst
                    && matches!(
                        defs.get(else_val).map(|&i| &block.instructions[i]),
                        Some(SIRInstruction::Mux(..))
                    )
                {
                    mux_else_children.insert(*else_val);
                }
            }

            for inst in &block.instructions {
                let SIRInstruction::Mux(dst, ..) = inst else {
                    continue;
                };
                if mux_else_children.contains(dst) {
                    continue;
                }

                let mut len = 0usize;
                let mut direct_case = 0usize;
                let mut acc_guarded_priority = 0usize;
                let mut cursor = Some(*dst);
                while let Some(reg) = cursor {
                    let Some(&idx) = defs.get(&reg) else {
                        break;
                    };
                    let SIRInstruction::Mux(_, cond, _, else_val) = &block.instructions[idx] else {
                        break;
                    };
                    len += 1;
                    if is_direct_case_eq(*cond, &defs, &block.instructions) {
                        direct_case += 1;
                    }
                    if is_acc_guarded_priority_cond(*cond, *else_val, &defs, &block.instructions) {
                        acc_guarded_priority += 1;
                    }
                    cursor = match defs.get(else_val).map(|&i| &block.instructions[i]) {
                        Some(SIRInstruction::Mux(..)) => Some(*else_val),
                        _ => None,
                    };
                }

                if len >= 4 {
                    rows.push((
                        len,
                        direct_case,
                        acc_guarded_priority,
                        eu_idx,
                        block.id,
                        *dst,
                    ));
                }
            }
        }
    }

    rows.sort_by(|a, b| b.cmp(a));
    for (rank, (len, direct_case, acc_guarded_priority, eu_idx, block_id, root)) in
        rows.into_iter().take(20).enumerate()
    {
        eprintln!(
            "[mux-chain-stats] rank={} eu={} block={} root=r{} len={} direct_case={} acc_guarded_priority={}",
            rank + 1,
            eu_idx,
            block_id.0,
            root.0,
            len,
            direct_case,
            acc_guarded_priority
        );
    }
}

fn is_direct_case_eq(
    cond: RegisterId,
    defs: &crate::HashMap<RegisterId, usize>,
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
) -> bool {
    let Some(&idx) = defs.get(&cond) else {
        return false;
    };
    match &instructions[idx] {
        SIRInstruction::Binary(_, lhs, BinaryOp::Eq | BinaryOp::EqWildcard, rhs) => {
            is_zero_mask_imm(*lhs, defs, instructions) || is_zero_mask_imm(*rhs, defs, instructions)
        }
        _ => false,
    }
}

fn is_acc_guarded_priority_cond(
    cond: RegisterId,
    prev_acc: RegisterId,
    defs: &crate::HashMap<RegisterId, usize>,
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
) -> bool {
    let Some(&idx) = defs.get(&cond) else {
        return false;
    };
    match &instructions[idx] {
        SIRInstruction::Binary(_, lhs, BinaryOp::LogicAnd, rhs) => {
            is_acc_eq_imm(*lhs, prev_acc, defs, instructions)
                || is_acc_eq_imm(*rhs, prev_acc, defs, instructions)
        }
        _ => false,
    }
}

fn is_acc_eq_imm(
    reg: RegisterId,
    prev_acc: RegisterId,
    defs: &crate::HashMap<RegisterId, usize>,
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
) -> bool {
    let Some(&idx) = defs.get(&reg) else {
        return false;
    };
    match &instructions[idx] {
        SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs) => {
            (*lhs == prev_acc && is_zero_mask_imm(*rhs, defs, instructions))
                || (*rhs == prev_acc && is_zero_mask_imm(*lhs, defs, instructions))
        }
        _ => false,
    }
}

fn is_zero_mask_imm(
    reg: RegisterId,
    defs: &crate::HashMap<RegisterId, usize>,
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
) -> bool {
    defs.get(&reg).is_some_and(|&idx| {
        matches!(
            &instructions[idx],
            SIRInstruction::Imm(_, value) if value.mask == num_bigint::BigUint::ZERO
        )
    })
}

/// Run program-wide and final-boundary combinational transforms.
///
/// These passes intentionally run after the main per-EU pipeline: several of
/// them depend on address aliases or CFG shapes produced by earlier passes.
/// Keeping the ordering in one named stage makes those dependencies explicit.
fn optimize_late_comb(
    program: &mut Program,
    opt: &crate::optimizer::OptimizeOptions,
    options: &PassOptions,
    unpacked_element_widths: &crate::HashMap<AbsoluteAddr, usize>,
) {
    let on = |pass: SirPass| opt.is_enabled(pass);
    let trace = std::env::var_os("CELOX_BRANCHIFY_STATS").is_some();
    let timing = std::env::var_os("CELOX_PASS_TIMING").is_some();
    let mut checkpoint = crate::timing::now();
    let verify_stage = |program: &Program, stage: &'static str| {
        if std::env::var_os("CELOX_SIR_VERIFY_PASSES").is_none() {
            return;
        }
        for (unit, eu) in program.eval_comb.iter().enumerate() {
            if let Err(error) = crate::parser::verify_memory_offset_contract(program, eu) {
                panic!("after late comb stage {stage} in unit {unit}: {error}");
            }
            if let Err(error) =
                pass_manager::verify_unpacked_element_boundaries(eu, unpacked_element_widths)
            {
                panic!("after late comb stage {stage} in unit {unit}: {error}");
            }
        }
    };
    macro_rules! checkpoint {
        ($name:literal) => {
            if timing {
                eprintln!("[late-comb-timing] {}: {:?}", $name, checkpoint.elapsed());
                checkpoint = crate::timing::now();
            }
        };
    }
    let branchify_watermarks = program
        .eval_comb
        .iter()
        .map(|eu| eu.blocks.keys().map(|block| block.0).max().unwrap_or(0))
        .collect::<Vec<_>>();

    // Identity Store bypass: share storage when B is unread; otherwise lower
    // a profitable exact copy directly from A's storage.
    if on(SirPass::IdentityStoreBypass) {
        let identity_aliases = pass_identity_store_bypass::optimize_program_identity_stores(
            program,
            options.four_state,
        );
        if !identity_aliases.is_empty() {
            program.address_aliases.extend(identity_aliases);
        }
    }
    verify_stage(program, "identity-store bypass");
    checkpoint!("identity-store bypass");
    if trace {
        eprintln!("[branchify-stats] late identity");
    }

    // Identity-store bypass can make an entire expression DAG dead.
    if opt.opt_level() != crate::optimizer::OptLevel::O0 {
        for eu in &mut program.eval_comb {
            pass_manager::ExecutionUnitPass::run(&LoopIdiomPass, eu, options);
        }
    }
    verify_stage(program, "loop idiom");
    checkpoint!("loop idiom");
    if trace {
        eprintln!("[branchify-stats] late loop");
    }
    if opt.opt_level() != crate::optimizer::OptLevel::O0 {
        let packed_scatter_store = PackedScatterStorePass::for_program(program);
        for eu in &mut program.eval_comb {
            pass_manager::ExecutionUnitPass::run(&packed_scatter_store, eu, options);
        }
    }
    verify_stage(program, "packed scatter");
    checkpoint!("packed scatter");
    if trace {
        eprintln!("[branchify-stats] late scatter");
    }
    if on(SirPass::IndexedStoreRecovery) {
        let indexed_store_recovery = IndexedStoreRecoveryPass::for_program(program);
        for eu in &mut program.eval_comb {
            pass_manager::ExecutionUnitPass::run(&indexed_store_recovery, eu, options);
        }
    }
    verify_stage(program, "indexed-store recovery");
    checkpoint!("indexed-store recovery");
    if trace {
        eprintln!("[branchify-stats] late indexed");
    }
    if opt.opt_level() != crate::optimizer::OptLevel::O0 {
        for eu in &mut program.eval_comb {
            pass_manager::ExecutionUnitPass::run(&GuardedRegionSinkingPass, eu, options);
        }
    }
    verify_stage(program, "guarded-region sinking");
    checkpoint!("guarded-region sinking");
    if trace {
        eprintln!("[branchify-stats] late guarded");
    }
    if opt.opt_level() != crate::optimizer::OptLevel::O0 {
        let sparse_case_pass = SparseCaseDispatchPass::new(&program.address_aliases);
        if trace {
            eprintln!("[branchify-stats] late sparse constructed");
        }
        for eu in &mut program.eval_comb {
            pass_manager::ExecutionUnitPass::run(&sparse_case_pass, eu, options);
        }
    }
    verify_stage(program, "sparse-case dispatch");
    checkpoint!("sparse-case dispatch");
    if trace {
        eprintln!("[branchify-stats] late sparse");
    }

    // Recover control dependence created by the program-wide transforms, then
    // repair value placement and correlated merge state on that final CFG.
    if on(SirPass::BranchifyMux) {
        for (eu, watermark) in program.eval_comb.iter_mut().zip(branchify_watermarks) {
            pass_branchify_mux::run_late_branchify_mux(eu, options, watermark);
        }
        for eu in &mut program.eval_comb {
            pass_guarded_region_sinking::sink_pure_values_with_predicate_repair(eu);
            pass_manager::ExecutionUnitPass::run(&PhiOutcomeCompressionPass, eu, options);
        }
    }
    verify_stage(program, "branchify and placement repair");
    checkpoint!("branchify and placement repair");

    // Final canonicalization must precede native lowering, which fixes live
    // ranges and spill slots.
    if on(SirPass::Gvn) {
        for eu in &mut program.eval_comb {
            pass_manager::ExecutionUnitPass::run(&GvnPass, eu, options);
        }
    }
    verify_stage(program, "final GVN");
    checkpoint!("final GVN");
    if on(SirPass::ControlFlowSimplify) {
        for eu in &mut program.eval_comb {
            pass_manager::ExecutionUnitPass::run(&ControlFlowSimplifyPass, eu, options);
        }
    }
    verify_stage(program, "final CFG simplify");
    checkpoint!("final CFG simplify");
    let _ = checkpoint;
}

fn optimize_with_options(
    program: &mut Program,
    max_inflight_loads: usize,
    four_state: bool,
    opt: &crate::optimizer::OptimizeOptions,
    preserve_element_storage_layout: bool,
) {
    #[cfg(not(target_arch = "wasm32"))]
    let timing = std::env::var("CELOX_PASS_TIMING").is_ok();
    #[cfg(target_arch = "wasm32")]
    let timing = false;
    let options = PassOptions {
        max_inflight_loads,
        four_state,
        optimize_options: opt.clone(),
        preserve_element_storage_layout,
    };
    let unpacked_element_widths = Arc::new(
        program
            .instance_module
            .iter()
            .flat_map(|(&instance_id, &module_id)| {
                program.module_variables[&module_id]
                    .values()
                    .filter_map(move |info| {
                        let element_count = info
                            .array_dims
                            .iter()
                            .try_fold(1usize, |count, &dimension| count.checked_mul(dimension))?;
                        (!info.array_dims.is_empty()
                            && element_count != 0
                            && info.width % element_count == 0)
                            .then_some((
                                AbsoluteAddr {
                                    instance_id,
                                    var_id: info.id,
                                },
                                info.width / element_count,
                            ))
                    })
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

    // Sparse next-state data must stay invisible until every evaluator for the
    // event has sampled STABLE.  Keep the commit in the same unified generated
    // function, but place it in a final EU after all evaluator EUs.
    move_sparse_commits_to_event_tail(&mut program.eval_apply_ffs);
    move_sparse_commits_to_event_tail(&mut program.eval_comb_apply_ffs);

    // 1. Unified Case (Fast Path): Full optimizations are safe.
    let phase_start = timing.then(crate::timing::now);
    if timing {
        for (trigger, units) in &program.eval_apply_ffs {
            for (index, eu) in units.iter().enumerate() {
                let instruction_count: usize = eu
                    .blocks
                    .values()
                    .map(|block| block.instructions.len())
                    .sum();
                eprintln!(
                    "[phase] eval_apply_ffs trigger={trigger} eu[{index}]: blocks={} insts={} regs={}",
                    eu.blocks.len(),
                    instruction_count,
                    eu.register_map.len()
                );
            }
        }
    }
    let mut ff_passes = ExecutionUnitPassManager::new()
        .with_unpacked_element_widths(Arc::clone(&unpacked_element_widths));
    // Note: EliminateWorkingRoundTripPass runs post-merge in emit_chained_eus
    // with boundary info for cross-EU independence check.
    // Per-EU elimination is NOT safe without dependency analysis.
    if on(SirPass::StoreLoadForwarding) {
        ff_passes.add_pass(StoreLoadForwardingPass);
    }
    if on(SirPass::ControlFlowSimplify) {
        ff_passes.add_pass(ControlFlowSimplifyPass);
    }
    if on(SirPass::Gvn) {
        ff_passes.add_pass(GvnPass);
        if on(SirPass::ControlFlowSimplify) {
            ff_passes.add_pass(PostGvnCfgCleanupPass);
        }
    }
    if on(SirPass::IndexedStoreRecovery) {
        ff_passes.add_pass(IndexedStoreRecoveryPass::for_program(program));
    }
    if on(SirPass::ConcatFolding) {
        ff_passes.add_pass(ConcatFoldingPass::new(Arc::clone(&unpacked_element_widths)));
    }
    if on(SirPass::XorChainFolding) {
        ff_passes.add_pass(XorChainFoldingPass);
    }
    if on(SirPass::HoistCommonBranchLoads) {
        ff_passes.add_pass(HoistCommonBranchLoadsPass);
    }
    if on(SirPass::BitExtractPeephole) {
        ff_passes.add_pass(BitExtractPeepholePass);
    }
    if on(SirPass::OptimizeBlocks) {
        ff_passes.add_pass(OptimizeBlocksPass {
            skip_final_schedule: on(SirPass::Reschedule),
            element_widths: Arc::clone(&element_widths),
        });
    }
    if on(SirPass::CoalesceStores) {
        ff_passes.add_pass(CoalesceStoresPass {
            element_widths: Arc::clone(&element_widths),
        });
    }
    if on(SirPass::SplitWideCommits) {
        ff_passes.add_pass(SplitWideCommitsPass);
    }

    // Fused comb+FF units retain the complete combinational producer graph.
    // Keep this pipeline independent from plain FF: comb-specific recovery
    // passes are both profitable and legal here without silently changing the
    // lowering contract of eval_apply_ff.
    let mut comb_ff_passes = ExecutionUnitPassManager::new()
        .with_unpacked_element_widths(Arc::clone(&unpacked_element_widths));
    // A shared comb/FF EU does not publish its intermediate comb state: the
    // deferred tick marks that state dirty and the next observation settles
    // comb again. Run this through the group cache so equivalent clock/reset
    // trigger bodies pay for StateSSA only once.
    comb_ff_passes.add_pass(FusedCombDsePass);
    if on(SirPass::StoreLoadForwarding) {
        comb_ff_passes.add_pass(StoreLoadForwardingPass);
        if on(SirPass::PartialForward) {
            comb_ff_passes.add_pass(PartialForwardPass);
        }
    }
    if on(SirPass::ControlFlowSimplify) {
        comb_ff_passes.add_pass(ControlFlowSimplifyPass);
    }
    if on(SirPass::Gvn) {
        comb_ff_passes.add_pass(GvnPass);
        if on(SirPass::ControlFlowSimplify) {
            comb_ff_passes.add_pass(PostGvnCfgCleanupPass);
        }
    }
    if on(SirPass::IndexedStoreRecovery) {
        comb_ff_passes.add_pass(IndexedStoreRecoveryPass::for_program(program));
    }
    if on(SirPass::ConcatFolding) {
        comb_ff_passes.add_pass(ConcatFoldingPass::new(Arc::clone(&unpacked_element_widths)));
    }
    if on(SirPass::XorChainFolding) {
        comb_ff_passes.add_pass(XorChainFoldingPass);
    }
    if on(SirPass::HoistCommonBranchLoads) {
        comb_ff_passes.add_pass(HoistCommonBranchLoadsPass);
    }
    if opt.opt_level() != crate::optimizer::OptLevel::O0 {
        comb_ff_passes.add_pass(GuardedRegionSinkingPass);
    }
    if on(SirPass::BranchifyMux) {
        comb_ff_passes.add_pass(BranchifyMuxPass);
        if opt.opt_level() != crate::optimizer::OptLevel::O0 {
            comb_ff_passes.add_pass(GuardedRegionSinkingPass);
        }
    }
    if on(SirPass::BitExtractPeephole) {
        comb_ff_passes.add_pass(BitExtractPeepholePass);
    }
    if opt.opt_level() != crate::optimizer::OptLevel::O0 {
        comb_ff_passes.add_pass(LoopIdiomPass);
    }
    if on(SirPass::OptimizeBlocks) {
        comb_ff_passes.add_pass(OptimizeBlocksPass {
            skip_final_schedule: on(SirPass::Reschedule),
            element_widths: Arc::clone(&element_widths),
        });
    }
    if on(SirPass::CoalesceStores) {
        comb_ff_passes.add_pass(CoalesceStoresPass {
            element_widths: Arc::clone(&element_widths),
        });
    }
    if on(SirPass::VectorizeConcat) {
        comb_ff_passes.add_pass(VectorizeConcatPass::new(Arc::clone(
            &unpacked_element_widths,
        )));
    }
    if opt.opt_level() != crate::optimizer::OptLevel::O0 {
        comb_ff_passes.add_pass(LoopIdiomPass);
    }
    if on(SirPass::MaskedArrayAny) {
        comb_ff_passes.add_pass(MaskedArrayAnyPass::for_program(program));
    }
    if on(SirPass::CircularPriority) {
        comb_ff_passes.add_pass(CircularPriorityPass::for_program(program));
    }

    let ff_eu_count: usize = program.eval_apply_ffs.values().map(Vec::len).sum();
    let comb_ff_eu_count: usize = program.eval_comb_apply_ffs.values().map(Vec::len).sum();
    let eu_count = ff_eu_count + comb_ff_eu_count;
    optimize_unit_groups_cached(&mut program.eval_apply_ffs, &ff_passes, &options);
    optimize_unit_groups_cached(&mut program.eval_comb_apply_ffs, &comb_ff_passes, &options);

    // The late comb pipeline must start from the CFG produced by the complete
    // initial pipeline. Keep it in the same exact-equivalence cache: clock
    // and reset triggers commonly share the complete fused body.
    let mut comb_ff_late_passes = ExecutionUnitPassManager::new()
        .with_unpacked_element_widths(Arc::clone(&unpacked_element_widths));
    if opt.opt_level() != crate::optimizer::OptLevel::O0 {
        comb_ff_late_passes.add_pass(GuardedRegionSinkingPass);
        comb_ff_late_passes.add_pass(SparseCaseDispatchPass::new(&program.address_aliases));
    }
    if on(SirPass::Gvn) {
        comb_ff_late_passes.add_pass(DeadCodeEliminationPass);
    }
    if on(SirPass::SplitWideCommits) {
        comb_ff_late_passes.add_pass(SplitWideCommitsPass);
    }
    optimize_unit_groups_cached(
        &mut program.eval_comb_apply_ffs,
        &comb_ff_late_passes,
        &options,
    );

    optimize_unified_commit_groups(
        &mut program.eval_apply_ffs,
        on(SirPass::CommitSinking),
        on(SirPass::InlineCommitForwarding),
    );
    optimize_unified_commit_groups(
        &mut program.eval_comb_apply_ffs,
        on(SirPass::CommitSinking),
        on(SirPass::InlineCommitForwarding),
    );
    let mut ff_post_passes = ExecutionUnitPassManager::new()
        .with_unpacked_element_widths(Arc::clone(&unpacked_element_widths));
    if on(SirPass::EliminateDeadWorkingStores) {
        ff_post_passes.add_pass(EliminateDeadWorkingStoresPass);
    }
    if on(SirPass::Reschedule) {
        ff_post_passes.add_pass(ReschedulePass);
    }
    // Coalescing reduces memory ops, but keeping a wide Concat live until its
    // Store can create unnecessary pressure.  Split it after scheduling.
    if on(SirPass::SplitCoalescedStores) {
        ff_post_passes.add_pass(SplitCoalescedStoresPass);
    }
    let mut comb_ff_post_passes = ExecutionUnitPassManager::new()
        .with_unpacked_element_widths(Arc::clone(&unpacked_element_widths));
    if on(SirPass::EliminateDeadWorkingStores) {
        comb_ff_post_passes.add_pass(EliminateDeadWorkingStoresPass);
    }
    if on(SirPass::Reschedule) {
        comb_ff_post_passes.add_pass(ReschedulePass);
    }
    if on(SirPass::SplitCoalescedStores) {
        comb_ff_post_passes.add_pass(SplitCoalescedStoresPass);
    }
    optimize_unit_groups_cached(&mut program.eval_apply_ffs, &ff_post_passes, &options);
    optimize_unit_groups_cached(
        &mut program.eval_comb_apply_ffs,
        &comb_ff_post_passes,
        &options,
    );
    if let Some(s) = phase_start {
        eprintln!("[phase] eval_apply_ffs ({eu_count} EUs): {:?}", s.elapsed());
    }

    // 2. Logic-Only Cache (Split Path Phase 1):
    // MUST NOT use EliminateDeadWorkingStoresPass because the Commits are in Phase 2.
    let phase_start = timing.then(crate::timing::now);
    let mut eval_only_passes = ExecutionUnitPassManager::new()
        .with_unpacked_element_widths(Arc::clone(&unpacked_element_widths));
    if on(SirPass::StoreLoadForwarding) {
        eval_only_passes.add_pass(StoreLoadForwardingPass);
    }
    if on(SirPass::ControlFlowSimplify) {
        eval_only_passes.add_pass(ControlFlowSimplifyPass);
    }
    if on(SirPass::Gvn) {
        eval_only_passes.add_pass(GvnPass);
        if on(SirPass::ControlFlowSimplify) {
            eval_only_passes.add_pass(PostGvnCfgCleanupPass);
        }
    }
    if on(SirPass::IndexedStoreRecovery) {
        eval_only_passes.add_pass(IndexedStoreRecoveryPass::for_program(program));
    }
    if on(SirPass::ConcatFolding) {
        eval_only_passes.add_pass(ConcatFoldingPass::new(Arc::clone(&unpacked_element_widths)));
    }
    if on(SirPass::XorChainFolding) {
        eval_only_passes.add_pass(XorChainFoldingPass);
    }
    if on(SirPass::HoistCommonBranchLoads) {
        eval_only_passes.add_pass(HoistCommonBranchLoadsPass);
    }
    if on(SirPass::BitExtractPeephole) {
        eval_only_passes.add_pass(BitExtractPeepholePass);
    }
    if on(SirPass::OptimizeBlocks) {
        eval_only_passes.add_pass(OptimizeBlocksPass {
            skip_final_schedule: on(SirPass::Reschedule),
            element_widths: Arc::clone(&element_widths),
        });
    }
    if on(SirPass::CoalesceStores) {
        eval_only_passes.add_pass(CoalesceStoresPass {
            element_widths: Arc::clone(&element_widths),
        });
    }
    if on(SirPass::Reschedule) {
        eval_only_passes.add_pass(ReschedulePass);
    }

    let eu_count: usize = program.eval_only_ffs.values().map(|v| v.len()).sum();
    optimize_unit_groups_cached(&mut program.eval_only_ffs, &eval_only_passes, &options);
    if let Some(s) = phase_start {
        eprintln!("[phase] eval_only_ffs ({eu_count} EUs): {:?}", s.elapsed());
    }

    // 3. Commit-Only Cache (Split Path Phase 2):
    let phase_start = timing.then(crate::timing::now);
    let mut apply_passes = ExecutionUnitPassManager::new()
        .with_unpacked_element_widths(Arc::clone(&unpacked_element_widths));
    if on(SirPass::StoreLoadForwarding) {
        apply_passes.add_pass(StoreLoadForwardingPass);
    }
    if on(SirPass::ControlFlowSimplify) {
        apply_passes.add_pass(ControlFlowSimplifyPass);
    }
    if on(SirPass::HoistCommonBranchLoads) {
        apply_passes.add_pass(HoistCommonBranchLoadsPass);
    }
    if on(SirPass::BitExtractPeephole) {
        apply_passes.add_pass(BitExtractPeepholePass);
    }
    if on(SirPass::OptimizeBlocks) {
        apply_passes.add_pass(OptimizeBlocksPass {
            skip_final_schedule: on(SirPass::Reschedule),
            element_widths: Arc::clone(&element_widths),
        });
    } // Still useful for loading from working memory
    if on(SirPass::CoalesceStores) {
        apply_passes.add_pass(CoalesceStoresPass {
            element_widths: Arc::clone(&element_widths),
        });
    }
    if on(SirPass::SplitWideCommits) {
        apply_passes.add_pass(SplitWideCommitsPass);
    }
    if on(SirPass::CommitSinking) {
        apply_passes.add_pass(CommitSinkingPass);
    }
    if on(SirPass::Reschedule) {
        apply_passes.add_pass(ReschedulePass);
    }

    let eu_count: usize = program.apply_ffs.values().map(|v| v.len()).sum();
    for units in program.apply_ffs.values_mut() {
        for eu in units {
            apply_passes.run(eu, &options);
        }
    }
    if let Some(s) = phase_start {
        eprintln!("[phase] apply_ffs ({eu_count} EUs): {:?}", s.elapsed());
    }

    // 4. Combinational Blocks:
    let phase_start = timing.then(crate::timing::now);
    let mut comb_passes = ExecutionUnitPassManager::new()
        .with_unpacked_element_widths(Arc::clone(&unpacked_element_widths));
    if on(SirPass::StoreLoadForwarding) {
        comb_passes.add_pass(StoreLoadForwardingPass);
        if on(SirPass::PartialForward) {
            comb_passes.add_pass(PartialForwardPass);
        }
    }
    if on(SirPass::ControlFlowSimplify) {
        comb_passes.add_pass(ControlFlowSimplifyPass);
    }
    if on(SirPass::Gvn) {
        comb_passes.add_pass(GvnPass);
        if on(SirPass::ControlFlowSimplify) {
            comb_passes.add_pass(ControlFlowSimplifyPass);
        }
    }
    if on(SirPass::ConcatFolding) {
        comb_passes.add_pass(ConcatFoldingPass::new(Arc::clone(&unpacked_element_widths)));
    }
    if on(SirPass::XorChainFolding) {
        comb_passes.add_pass(XorChainFoldingPass);
    }
    if on(SirPass::HoistCommonBranchLoads) {
        comb_passes.add_pass(HoistCommonBranchLoadsPass);
    }
    if opt.opt_level() != crate::optimizer::OptLevel::O0 {
        // Recover coupled observable outputs while their shared producer DAG
        // is still intact.  BranchifyMux may otherwise split an inner value
        // diamond first and hide the common result/flags control region behind
        // block parameters.
        comb_passes.add_pass(GuardedRegionSinkingPass);
    }
    if on(SirPass::BranchifyMux) {
        comb_passes.add_pass(BranchifyMuxPass);
        if opt.opt_level() != crate::optimizer::OptLevel::O0 {
            comb_passes.add_pass(GuardedRegionSinkingPass);
        }
    }
    if on(SirPass::BitExtractPeephole) {
        comb_passes.add_pass(BitExtractPeepholePass);
    }
    if opt.opt_level() != crate::optimizer::OptLevel::O0 {
        comb_passes.add_pass(LoopIdiomPass);
    }
    if on(SirPass::OptimizeBlocks) {
        comb_passes.add_pass(OptimizeBlocksPass {
            skip_final_schedule: false, // eval_comb has no reschedule pass
            element_widths: Arc::clone(&element_widths),
        });
    }
    if on(SirPass::CoalesceStores) {
        comb_passes.add_pass(CoalesceStoresPass {
            element_widths: Arc::clone(&element_widths),
        });
    }
    if on(SirPass::VectorizeConcat) {
        comb_passes.add_pass(VectorizeConcatPass::new(Arc::clone(
            &unpacked_element_widths,
        )));
    }
    if opt.opt_level() != crate::optimizer::OptLevel::O0 {
        // Vectorization exposes the wide source of predicate concats.  A
        // second idiom/DCE sweep removes the scalar predicates it replaced.
        comb_passes.add_pass(LoopIdiomPass);
    }
    if on(SirPass::MaskedArrayAny) {
        comb_passes.add_pass(MaskedArrayAnyPass::for_program(program));
    }
    if on(SirPass::CircularPriority) {
        comb_passes.add_pass(CircularPriorityPass::for_program(program));
    }
    if on(SirPass::Gvn) {
        comb_passes.add_pass(GvnPass);
        if on(SirPass::ControlFlowSimplify) {
            comb_passes.add_pass(PostGvnCfgCleanupPass);
        }
        // GVN removes only redundant definitions. Transformations above also
        // leave ordinary dead pure chains, so finish with explicit mark/sweep
        // DCE instead of relying on CFG cleanup to happen to remove them.
        comb_passes.add_pass(DeadCodeEliminationPass);
    }

    let eu_count = program.eval_comb.len();
    for (i, eu) in program.eval_comb.iter_mut().enumerate() {
        if timing {
            let inst_count: usize = eu.blocks.values().map(|b| b.instructions.len()).sum();
            let block_count = eu.blocks.len();
            eprintln!("[phase] eval_comb eu[{i}]: blocks={block_count} insts={inst_count}");
        }
        comb_passes.run(eu, &options);
    }
    if let Some(s) = phase_start {
        eprintln!("[phase] eval_comb ({eu_count} EUs): {:?}", s.elapsed());
    }

    optimize_late_comb(program, opt, &options, &unpacked_element_widths);
    if std::env::var_os("CELOX_MUX_CHAIN_STATS").is_some() {
        dump_mux_chain_stats(&program.eval_comb);
    }

    // 5. Tail-call chain splitting for eval_comb.
    // When the estimated CLIF instruction count exceeds Cranelift's limit,
    // split into a chain of smaller functions connected by tail calls.
    //
    // Try EU-boundary / single-block splitting first (zero live-reg cost).
    // Fall back to memory-spilled multi-block splitting if needed.
    if on(SirPass::TailCallSplit) {
        if timing {
            for (i, eu) in program.eval_comb.iter().enumerate() {
                let inst_cost = cost_model::estimate_eu_cost(eu, four_state);
                let value_count = cost_model::estimate_eu_value_count(eu, four_state);
                eprintln!(
                    "[split-check] eval_comb eu[{i}]: blocks={} insts={} clif_cost={inst_cost}/{} values={value_count}/{}",
                    eu.blocks.len(),
                    eu.blocks
                        .values()
                        .map(|b| b.instructions.len())
                        .sum::<usize>(),
                    cost_model::CLIF_INST_THRESHOLD,
                    cost_model::VREG_VALUE_THRESHOLD,
                );
            }
        }
        let split_start = timing.then(crate::timing::now);
        if let Some(chunks) = pass_tail_call_split::split_if_needed(&program.eval_comb, four_state)
        {
            if timing {
                eprintln!(
                    "[split] TailCallChunks: {} chunks, took {:?}",
                    chunks.len(),
                    split_start.unwrap().elapsed()
                );
            }
            program.eval_comb_plan = Some(crate::ir::EvalCombPlan::TailCallChunks(chunks));
        } else if let Some(plan) =
            pass_tail_call_split::split_if_needed_spilled(&program.eval_comb, four_state)
        {
            if timing {
                eprintln!(
                    "[split] MemorySpilled: {} chunks, scratch={}B, took {:?}",
                    plan.chunks.len(),
                    plan.scratch_bytes,
                    split_start.unwrap().elapsed()
                );
                for (i, chunk) in plan.chunks.iter().enumerate() {
                    let blocks = chunk.eu.blocks.len();
                    let insts: usize = chunk.eu.blocks.values().map(|b| b.instructions.len()).sum();
                    eprintln!(
                        "[split]   chunk[{i}]: blocks={blocks} insts={insts} in_spills={} out_spills={} cross_edges={}",
                        chunk.incoming_spills.len(),
                        chunk.outgoing_spills.len(),
                        chunk.cross_chunk_edges.len()
                    );
                }
            }
            program.eval_comb_plan = Some(crate::ir::EvalCombPlan::MemorySpilled(plan));
        }
    }
}
