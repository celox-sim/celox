//! AArch64 instruction selection: lowers SIR (bit-level SSA) directly to the
//! target-owned scalar MIR.
//!
//! Supports 2-state and 4-state (IEEE 1800) with full mask propagation.
//! Handles arbitrary widths: narrow (≤64-bit) and wide (>64-bit, chunk-based).
//!
//! This module owns execution-unit planning and the shared lowering context.
//! Instruction dispatch lives in `instruction`; operand lowering is grouped into
//! `memory`, `concat`, `wide`, `wide_unary`, `bit_count`, and `four_state`.
//! Runtime event writes and capture notifications live in `runtime_events`.

mod bit_count;
mod concat;
mod dynamic_load_cache;
mod extern_call;
mod four_state;
mod instruction;
mod memory;
mod packed_compare;
mod runtime_events;
mod sparse;
mod strided;
mod wide;
mod wide_unary;

use bit_count::{lower_narrow_bit_count, lower_wide_bit_count};
use concat::{
    lower_flat_concat_to_chunks, match_guarded_cmp_select_cond, try_lower_concat_of_muxes,
    try_lower_repeated_msb_concat,
};
use four_state::{
    get_wide_mask_chunks, lower_binary_mask, lower_four_state_mux_chunk, lower_mux_condition_state,
    lower_slice_mask, lower_unary_mask, lower_wide_binary_mask, lower_wide_to_two_state,
    lower_wide_unary_mask, normalize_wide_value,
};
use instruction::lower_instruction;
use memory::{
    direct_element_byte_offset, emit_aligned_dynamic_wide_store,
    emit_dynamic_scalar_bitfield_store, emit_dynamic_wide_bitfield_store, emit_state_zero_fill,
    emit_static_commit_plane, lower_block_cached_dynamic_load, lower_dynamic_wide_load_chunks,
    lower_static_wide_load_chunks, memory_offset_low_zero_bits, memory_offset_vreg,
    prepare_sparse_store, recomposed_element_byte_offset, try_emit_single_chunk_sparse_store,
};
use runtime_events::{
    collect_static_comb_store_byte_probes, emit_enable_comb_capture_sites,
    emit_enable_comb_capture_sites_if_byte_probes_changed,
    emit_enable_comb_capture_sites_if_regs_changed, load_runtime_event_ptr,
    load_runtime_event_ptr_and_comb_capture_enabled, lower_runtime_event_write,
};
use wide::{ShiftDir, lower_wide_binary, lower_wide_runtime_shift_chunks, wide_reduce_or};
use wide_unary::{lower_wide_extract, lower_wide_unary};

use super::mir::*;
use super::sparse_write_state::{
    SparseChunkState, SparseMetadataAction, SparseWriteState, SparseWriteStates,
};
use crate::MemoryLayout;
use crate::{
    BasicBlock, BinaryOp, ExecutionUnit, RegisterId, RegisterType, SIRInstruction, SIROffset,
    SIRTerminator, UnaryOp,
};
use crate::{HashMap, HashSet};
use crate::{RegionedAbsoluteAddr, STABLE_REGION};
use celox_sir::analysis::{
    ExactU64Constant as ExactSirConstant, UseSite as SirUseSite,
    block_instruction_definitions as collect_sir_defs,
    collect_unique_exact_u64_constants as collect_exact_sir_constants,
    collect_use_sites as collect_sir_use_sites, instruction_definition as sir_def_reg,
    reverse_postorder as ordered_sir_blocks, visit_instruction_uses as collect_sir_inst_uses,
};
use dynamic_load_cache::{block_dynamic_load_cache_plans, native_plane_access_size};
use packed_compare::{
    PackedByteAffineComparePlans, PackedLaneComparePlanRhs, PackedLaneComparePlans,
    find_packed_byte_affine_compare_plans, find_packed_lane_compare_plans,
};
use sparse::{find_sparse_worklist_run, sparse_descriptor_table};

/// Maps SIR RegisterId → MIR VReg for the current execution unit.
struct RegMap {
    map: Vec<Option<VReg>>,
}

impl RegMap {
    fn new(capacity: usize) -> Self {
        Self {
            map: vec![None; capacity],
        }
    }

    fn get(&self, reg: RegisterId) -> VReg {
        self.map[reg.0].unwrap_or_else(|| panic!("SIR register r{} not yet defined", reg.0))
    }

    fn set(&mut self, reg: RegisterId, vreg: VReg) {
        self.map[reg.0] = Some(vreg);
    }
}

#[derive(Debug, Clone, Copy)]
struct BlockDynamicLoadCacheEntry {
    value: VReg,
    mask: Option<VReg>,
}

/// Lower a single SIR execution unit to a MIR function.
pub fn lower_execution_unit(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
    four_state: bool,
) -> MFunction {
    lower_execution_unit_with_diagnostics(
        eu,
        layout,
        four_state,
        &crate::NativeDiagnostics::default(),
    )
}

pub fn lower_execution_unit_with_diagnostics(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
    four_state: bool,
    diagnostics: &crate::NativeDiagnostics,
) -> MFunction {
    let expanded = strided::expand_strided_accesses(eu, layout);
    let eu = expanded.as_ref();
    if cfg!(debug_assertions) || diagnostics.verify_sir {
        if let Err(error) = eu.verify_result() {
            panic!("before native ISel: {error}");
        }
    }
    let mut vregs = VRegAllocator::new();
    let mut spill_descs: Vec<SpillDesc> = Vec::new();
    let max_sir_regs = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0) + 1;
    let mut reg_map = RegMap::new(max_sir_regs);
    let trace_regs = diagnostics
        .isel_trace_regs
        .iter()
        .copied()
        .map(RegisterId)
        .collect::<HashSet<_>>();
    let mut sir_registers = eu.register_map.keys().copied().collect::<Vec<_>>();
    sir_registers.sort_unstable_by_key(|register| register.0);

    // Pre-allocate a VReg for each SIR register
    for sir_reg_id in &sir_registers {
        let vreg = vregs.alloc();
        reg_map.set(*sir_reg_id, vreg);
        if trace_regs.contains(sir_reg_id) {
            tracing::debug!("[isel-trace] prealloc r{} -> {}", sir_reg_id.0, vreg);
        }
        // Spill desc will be filled during instruction lowering.
        // For now, default to transient.
        spill_descs.push(SpillDesc::transient());
    }

    let mut func = MFunction::for_isel(vregs.clone(), spill_descs);
    let mut block_ids = ordered_sir_blocks(eu);
    let sparse_worklist_run = find_sparse_worklist_run(eu);
    let sparse_write_states = match sparse_worklist_run {
        Some((commit_block, commit_start, _)) => {
            SparseWriteStates::analyze(eu, layout, commit_block, commit_start)
                .unwrap_or_else(|| SparseWriteStates::zero_fills_only(eu, layout))
        }
        None => SparseWriteStates::zero_fills_only(eu, layout),
    };
    let sparse_descriptor_table = sparse_worklist_run
        .is_some()
        .then(|| func.intern_constant_table(sparse_descriptor_table(layout)));
    let native_priority_encode = !four_state;
    let sir_use_sites = if native_priority_encode {
        Some(collect_sir_use_sites(eu))
    } else {
        None
    };
    let exact_constants = (!four_state).then(|| collect_exact_sir_constants(eu));
    let selector_branch_table_plans = if !four_state {
        find_selector_branch_table_plans(
            eu,
            exact_constants
                .as_ref()
                .expect("two-state branch tables require exact constants"),
            sir_use_sites
                .as_ref()
                .expect("two-state branch tables require SIR uses"),
        )
    } else {
        SelectorBranchTablePlans::default()
    };
    block_ids.retain(|block| !selector_branch_table_plans.removed_blocks.contains(block));
    let mut dense_lookup_plans_by_block: HashMap<crate::BlockId, DenseLookupPlans> =
        HashMap::default();
    if !four_state {
        let uses = sir_use_sites
            .as_ref()
            .expect("two-state lookup planning must collect SIR uses");
        let constants = exact_constants
            .as_ref()
            .expect("two-state lowering must collect exact constants");
        for &block_id in &block_ids {
            let block = &eu.blocks[&block_id];
            let mut plans = find_dense_lookup_plans(block, &eu.register_map, constants, uses);
            let mut root_indices: Vec<_> = plans.roots.keys().copied().collect();
            root_indices.sort_unstable();
            for root_idx in root_indices {
                let plan = plans
                    .roots
                    .get_mut(&root_idx)
                    .expect("collected dense lookup root must still exist");
                plan.table = Some(func.intern_constant_table(plan.entries.clone()));
            }
            if !plans.roots.is_empty() {
                dense_lookup_plans_by_block.insert(block_id, plans);
            }
        }
    }

    let mut next_extra_block_id = block_ids.iter().map(|bid| bid.0).max().unwrap_or(0) + 1;
    let mut sir_exit_mir_blocks: HashMap<crate::BlockId, BlockId> = HashMap::default();

    let mut mask_map = RegMap::new(max_sir_regs);
    // Pre-allocate mask VRegs for 4-state
    if four_state {
        for sir_reg_id in &sir_registers {
            let mvreg = func.vregs.alloc();
            mask_map.set(*sir_reg_id, mvreg);
            func.spill_descs.push(SpillDesc::transient());
        }
    }

    let mut ctx = ISelContext {
        vregs: &mut func.vregs,
        spill_descs: &mut func.spill_descs,
        reg_map: &mut reg_map,
        register_types: &eu.register_map,
        layout,
        wide_regs: WideRegMap::default(),
        reg_addrs: crate::HashMap::default(),
        consts: ConstMap::default(),
        low_zero_bits: crate::HashMap::default(),
        four_state,
        mask_map,
        known_bits: crate::HashMap::default(),
        wide_masks: WideMaskMap::default(),
        trigger_only_seen: HashSet::default(),
        sparse_descriptor_table,
        trace_regs,
    };
    // Pre-seed wide block params so instructions in those blocks can read the
    // full chunked value before phi nodes are materialized in a later pass.
    for &sir_block_id in &block_ids {
        let sir_block = &eu.blocks[&sir_block_id];
        for &param_reg in &sir_block.params {
            let width = eu.register_map[&param_reg].width();
            let num_chunks = width.div_ceil(64).max(1);
            if num_chunks <= 1 {
                continue;
            }
            if !ctx.wide_regs.contains_key(&param_reg) {
                let mut chunks = Vec::with_capacity(num_chunks);
                chunks.push((ctx.reg_map.get(param_reg), width.min(64)));
                for chunk_idx in 1..num_chunks {
                    let chunk_width = (width - chunk_idx * 64).min(64);
                    let vreg = ctx.alloc_vreg(SpillDesc::transient());
                    chunks.push((vreg, chunk_width));
                }
                ctx.set_wide_chunks(param_reg, chunks);
            }
            if four_state {
                if !ctx.wide_masks.contains_key(&param_reg) {
                    let mut chunks = Vec::with_capacity(num_chunks);
                    let mask0 = ctx.mask_map.get(param_reg);
                    chunks.push((mask0, width.min(64)));
                    for chunk_idx in 1..num_chunks {
                        let chunk_width = (width - chunk_idx * 64).min(64);
                        let vreg = ctx.alloc_vreg(SpillDesc::transient());
                        chunks.push((vreg, chunk_width));
                    }
                    ctx.wide_masks.insert(param_reg, chunks);
                }
            }
        }
    }

    // Collect mask phi sources per-block (captures mask state at each terminator)
    let mut mask_phi_sources: HashMap<BlockId, Vec<(BlockId, usize, usize, VReg)>> =
        HashMap::default();

    for &sir_block_id in &block_ids {
        let sir_block = &eu.blocks[&sir_block_id];
        let mir_block_id = BlockId(sir_block_id.0 as u32);
        let mut mblock = MBlock::new(mir_block_id);
        ctx.trigger_only_seen.clear();

        // Record static Load origins before lowering this block so Slice can
        // reload the same range after an intervening partial Store.
        for inst in &sir_block.instructions {
            if let SIRInstruction::Load(dst, addr, SIROffset::Static(bit_offset), _) = inst {
                ctx.reg_addrs.insert(*dst, (*addr, *bit_offset));
            }
        }

        let priority_plans = if native_priority_encode {
            find_priority_encode_plans(sir_block, sir_use_sites.as_ref().unwrap())
        } else {
            PriorityEncodePlans::default()
        };
        let lookup_plans = dense_lookup_plans_by_block
            .remove(&sir_block_id)
            .unwrap_or_default();
        let branch_table_plan = selector_branch_table_plans.roots.get(&sir_block_id);
        let packed_lane_compare_plans = if !four_state {
            find_packed_lane_compare_plans(
                sir_block,
                &eu.register_map,
                exact_constants
                    .as_ref()
                    .expect("two-state packed compares must collect exact constants"),
                layout,
                sir_use_sites
                    .as_ref()
                    .expect("two-state packed compares must collect SIR uses"),
            )
        } else {
            PackedLaneComparePlans::default()
        };
        let packed_byte_affine_compare_plans = if !four_state {
            find_packed_byte_affine_compare_plans(
                sir_block,
                &eu.register_map,
                exact_constants
                    .as_ref()
                    .expect("two-state packed compares must collect exact constants"),
                sir_use_sites
                    .as_ref()
                    .expect("two-state packed compares must collect SIR uses"),
            )
        } else {
            PackedByteAffineComparePlans::default()
        };
        let dynamic_load_cache_plans = block_dynamic_load_cache_plans(sir_block, layout);
        let mut dynamic_load_cache = HashMap::default();
        let mut lookup_emit_cache = DenseLookupEmitCache::default();
        let sir_defs = collect_sir_defs(sir_block);

        // Waveform observers share Store/Commit notification sites with clock
        // triggers. Mark before specialized store/commit lowering can absorb a
        // run (packed stores and sparse worklists included). Repeated writes
        // in this basic block need only one notification per physical group.
        let mut trace_marks = HashSet::default();
        // Lower instructions
        for (inst_idx, inst) in sir_block.instructions.iter().enumerate() {
            let target = match inst {
                SIRInstruction::Store(addr, _, width, _, _, _)
                | SIRInstruction::Commit(_, addr, _, width, _)
                    if *width != 0 =>
                {
                    Some(addr)
                }
                _ => None,
            };
            if let Some(offsets) = target.and_then(|addr| layout.trace_notification_offsets(addr)) {
                for offset in offsets {
                    if trace_marks.insert(offset) {
                        let one = ctx.alloc_vreg(SpillDesc::remat(1));
                        mblock.push(MInst::LoadImm { dst: one, value: 1 });
                        mblock.push(MInst::Store {
                            base: BaseReg::SimState,
                            offset: offset as i32,
                            src: one,
                            size: OpSize::S8,
                        });
                    }
                }
            }
            if branch_table_plan.is_some_and(|plan| plan.skip_indices.contains(&inst_idx)) {
                continue;
            }
            if let Some((worklist_block, start, end)) = sparse_worklist_run
                && sir_block_id == worklist_block
                && (start..end).contains(&inst_idx)
            {
                if inst_idx == start {
                    mblock.push(MInst::SparseCommitWorklist {
                        descriptor_table: sparse_descriptor_table
                            .expect("planned sparse worklist must have descriptor table"),
                        active_bits_offset: layout.sparse_active_bits_offset as i32,
                        active_capacity: layout.sparse_active_capacity,
                    });
                }
                continue;
            }
            if sparse_write_states.is_dead_zero_definition(sir_block_id, inst_idx) {
                continue;
            }
            if sparse_write_states.is_zero_fill_member(sir_block_id, inst_idx) {
                if let Some(address) = sparse_write_states.zero_fill_root(sir_block_id, inst_idx) {
                    emit_state_zero_fill(&mut ctx, &mut mblock, address);
                }
                continue;
            }
            if let Some(dst) = sir_def_reg(inst)
                && ctx.trace_regs.contains(&dst)
            {
                tracing::debug!(
                    "[isel-trace] b{} inst {} lowering r{}: {}",
                    sir_block.id.0,
                    inst_idx,
                    dst.0,
                    inst
                );
            }
            if packed_lane_compare_plans.skip_indices.contains(&inst_idx) {
                if let Some(plan) = packed_lane_compare_plans.roots.get(&inst_idx) {
                    let offset = ctx.byte_offset(&plan.address, 0);
                    let byte_len = plan.lane_count * plan.element_stride;
                    let rhs = match plan.rhs {
                        PackedLaneComparePlanRhs::Scalar(value) => {
                            PackedLaneCompareRhs::Scalar(ctx.reg_map.get(value))
                        }
                        PackedLaneComparePlanRhs::Memory(address) => {
                            let rhs_offset = ctx.byte_offset(&address, 0);
                            PackedLaneCompareRhs::Memory {
                                offset: rhs_offset,
                                alias_range: MemoryAliasRange::new(rhs_offset, byte_len),
                            }
                        }
                    };
                    mblock.push(MInst::PackedLaneCompare {
                        dst: ctx.reg_map.get(plan.dst),
                        rhs,
                        kind: plan.kind,
                        offset,
                        lane_count: plan.lane_count as u8,
                        element_stride: plan.element_stride as u8,
                        bit_offset: plan.bit_offset as u8,
                        field_width: plan.field_width as u8,
                        alias_range: MemoryAliasRange::new(offset, byte_len),
                    });
                    ctx.known_bits
                        .insert(ctx.reg_map.get(plan.dst), plan.lane_count);
                }
                continue;
            }
            if packed_byte_affine_compare_plans
                .skip_indices
                .contains(&inst_idx)
            {
                if let Some(plan) = packed_byte_affine_compare_plans.roots.get(&inst_idx) {
                    mblock.push(MInst::PackedByteAffineCompare {
                        dst: ctx.reg_map.get(plan.dst),
                        base: ctx.reg_map.get(plan.base),
                        rhs: ctx.reg_map.get(plan.rhs),
                        kind: plan.kind,
                    });
                    ctx.known_bits.insert(ctx.reg_map.get(plan.dst), 16);
                }
                continue;
            }
            if lookup_plans.skip_indices.contains(&inst_idx) {
                if let Some(plan) = lookup_plans.roots.get(&inst_idx) {
                    if ctx.trace_regs.contains(&plan.dst) {
                        tracing::debug!(
                            "[isel-trace] b{} inst {} dense-lookup root r{} selector=r{} entries={}",
                            sir_block.id.0,
                            inst_idx,
                            plan.dst.0,
                            plan.selector.0,
                            plan.entries.len(),
                        );
                    }
                    emit_dense_lookup(&mut ctx, &mut mblock, plan, &mut lookup_emit_cache);
                }
                continue;
            }
            if priority_plans.skip_indices.contains(&inst_idx) {
                if let Some(plan) = priority_plans.roots.get(&inst_idx) {
                    if ctx.trace_regs.contains(&plan.dst) {
                        tracing::debug!(
                            "[isel-trace] b{} inst {} priority-encode root r{} -> {}",
                            sir_block.id.0,
                            inst_idx,
                            plan.dst.0,
                            ctx.reg_map.get(plan.dst)
                        );
                    }
                    emit_priority_encode(&mut ctx, &mut mblock, plan);
                } else if let Some(dst) = sir_def_reg(inst)
                    && ctx.trace_regs.contains(&dst)
                {
                    tracing::debug!(
                        "[isel-trace] b{} inst {} skipped r{} without root",
                        sir_block.id.0,
                        inst_idx,
                        dst.0
                    );
                }
                continue;
            }

            if let SIRInstruction::CombCaptureEvent {
                site_id,
                args,
                fatal_error_code,
                consume_enabled,
            } = inst
            {
                let (event_ptr, enabled) = load_runtime_event_ptr_and_comb_capture_enabled(
                    &mut ctx,
                    &mut mblock,
                    *site_id,
                );
                let write_block_id = BlockId(next_extra_block_id as u32);
                next_extra_block_id += 1;
                let cont_block_id = BlockId(next_extra_block_id as u32);
                next_extra_block_id += 1;

                mblock.push(MInst::Branch {
                    cond: enabled,
                    true_bb: write_block_id,
                    false_bb: cont_block_id,
                });
                func.blocks.push(mblock);

                let mut write_block = MBlock::new(write_block_id);
                lower_runtime_event_write(&mut ctx, &mut write_block, event_ptr, *site_id, args);
                if *consume_enabled {
                    let enabled_ptr = ctx.alloc_vreg(SpillDesc::transient());
                    write_block.push(MInst::Load {
                        dst: enabled_ptr,
                        base: BaseReg::SimState,
                        offset: celox_state_layout::STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET
                            as i32,
                        size: OpSize::S64,
                    });
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    write_block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    write_block.push(MInst::StorePtr {
                        ptr: enabled_ptr,
                        offset: *site_id as i32,
                        src: zero,
                        size: OpSize::S8,
                    });
                }
                if let Some(code) = fatal_error_code {
                    write_block.push(MInst::ReturnError { code: *code });
                } else {
                    write_block.push(MInst::Jump {
                        target: cont_block_id,
                    });
                }
                func.blocks.push(write_block);

                mblock = MBlock::new(cont_block_id);
            } else if let SIRInstruction::Load(destination, address, offset, width) = inst
                && dynamic_load_cache_plans.addresses.contains(address)
                && matches!(offset, SIROffset::Dynamic(_) | SIROffset::Element { .. })
            {
                lower_block_cached_dynamic_load(
                    &mut ctx,
                    &mut mblock,
                    *destination,
                    *address,
                    offset,
                    *width,
                    &mut dynamic_load_cache,
                );
            } else {
                lower_instruction(
                    &mut ctx,
                    &mut mblock,
                    inst,
                    sir_block,
                    &sir_defs,
                    sparse_write_states.state(sir_block_id, inst_idx),
                    sparse_write_states.chunk_state(sir_block_id, inst_idx),
                    sparse_write_states.dirty_word_state(sir_block_id, inst_idx),
                    sparse_write_states.metadata_action(sir_block_id, inst_idx),
                );
            }
            if cfg!(debug_assertions) {
                ctx.verify_wide_values();
            }

            // Track known bit width for redundant mask elimination.
            let dst_reg = inst.defined_register();
            if let Some(dr) = dst_reg {
                let w = ctx.sir_width(&dr);
                if w <= 64 {
                    let vreg = ctx.reg_map.get(dr);
                    ctx.known_bits.insert(vreg, w);
                    if ctx.trace_regs.contains(&dr) {
                        tracing::debug!(
                            "[isel-trace] b{} inst {} after r{} -> {} known_bits={}",
                            sir_block.id.0,
                            inst_idx,
                            dr.0,
                            vreg,
                            w
                        );
                    }
                }
            }
        }

        // Lower terminator
        if let Some(plan) = branch_table_plan {
            lower_selector_branch_table(&mut ctx, &mut mblock, plan);
        } else {
            lower_terminator(&mut ctx, &mut mblock, &sir_block.terminator);
        }
        let pred_mir_id = mblock.id;
        sir_exit_mir_blocks.insert(sir_block_id, pred_mir_id);

        // Capture mask phi sources from this block's terminator (before mask_map changes)
        if four_state {
            let edges: Vec<(crate::BlockId, &[RegisterId])> = match &sir_block.terminator {
                SIRTerminator::Jump(target, args) => vec![(*target, args.as_slice())],
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => vec![
                    (true_block.0, true_block.1.as_slice()),
                    (false_block.0, false_block.1.as_slice()),
                ],
                SIRTerminator::Switch { .. } => Vec::new(),
                _ => vec![],
            };
            for (target_sir_id, args) in edges {
                if args.is_empty() {
                    continue;
                }
                let target_mir_id = BlockId(target_sir_id.0 as u32);
                for (i, arg_reg) in args.iter().enumerate() {
                    if let Some(mask_chunks) = ctx.wide_masks.get(arg_reg) {
                        for (chunk_idx, (mask_vreg, _)) in mask_chunks.iter().enumerate() {
                            mask_phi_sources.entry(target_mir_id).or_default().push((
                                pred_mir_id,
                                i,
                                chunk_idx,
                                *mask_vreg,
                            ));
                        }
                    } else if let Some(mask_vreg) =
                        ctx.mask_map.map.get(arg_reg.0).copied().flatten()
                    {
                        mask_phi_sources.entry(target_mir_id).or_default().push((
                            pred_mir_id,
                            i,
                            0,
                            mask_vreg,
                        ));
                    }
                }
            }
        }

        func.blocks.push(mblock);
    }

    // Extract mask_map for phi node construction (ctx borrows func fields)
    let saved_mask_map = std::mem::replace(&mut ctx.mask_map, RegMap::new(0));
    let saved_wide_regs = std::mem::take(&mut ctx.wide_regs);
    let saved_wide_masks = std::mem::take(&mut ctx.wide_masks);
    drop(ctx); // Release borrows on func

    // Build phi nodes from SIR block params and predecessor terminators.
    // For each SIR block with params, find all predecessors that pass args.
    {
        use crate::HashMap;
        // Collect phi sources: target_block → [(pred_block, param_idx, chunk_idx, arg_vreg)]
        let mut phi_sources: HashMap<BlockId, Vec<(BlockId, usize, usize, VReg)>> =
            HashMap::default();
        for &sir_block_id in &block_ids {
            let sir_block = &eu.blocks[&sir_block_id];
            let pred_mir_id = sir_exit_mir_blocks
                .get(&sir_block_id)
                .copied()
                .unwrap_or(BlockId(sir_block_id.0 as u32));
            let edges: Vec<(crate::BlockId, &[RegisterId])> = match &sir_block.terminator {
                SIRTerminator::Jump(target, args) => vec![(*target, args.as_slice())],
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => vec![
                    (true_block.0, true_block.1.as_slice()),
                    (false_block.0, false_block.1.as_slice()),
                ],
                SIRTerminator::Switch { .. } => Vec::new(),
                _ => vec![],
            };
            for (target_sir_id, args) in edges {
                if args.is_empty() {
                    continue;
                }
                let target_mir_id = BlockId(target_sir_id.0 as u32);
                for (i, arg_reg) in args.iter().enumerate() {
                    if let Some(chunks) = saved_wide_regs.get(arg_reg) {
                        for (chunk_idx, (arg_vreg, _)) in chunks.iter().enumerate() {
                            phi_sources.entry(target_mir_id).or_default().push((
                                pred_mir_id,
                                i,
                                chunk_idx,
                                *arg_vreg,
                            ));
                        }
                    } else {
                        let arg_vreg = reg_map.get(*arg_reg);
                        phi_sources.entry(target_mir_id).or_default().push((
                            pred_mir_id,
                            i,
                            0,
                            arg_vreg,
                        ));
                    }
                }
            }
        }
        // Build phi nodes on target blocks
        for mblock in &mut func.blocks {
            if let Some(sources) = phi_sources.remove(&mblock.id) {
                let sir_block_id = crate::BlockId(mblock.id.0 as usize);
                let sir_block = &eu.blocks[&sir_block_id];
                for (param_idx, param_reg) in sir_block.params.iter().enumerate() {
                    if let Some(dst_chunks) = saved_wide_regs.get(param_reg) {
                        for (chunk_idx, (dst, _)) in dst_chunks.iter().enumerate() {
                            let phi_srcs: Vec<(BlockId, VReg)> = sources
                                .iter()
                                .filter(|(_, idx, src_chunk_idx, _)| {
                                    *idx == param_idx && *src_chunk_idx == chunk_idx
                                })
                                .map(|(pred, _, _, vreg)| (*pred, *vreg))
                                .collect();
                            if !phi_srcs.is_empty() {
                                mblock.phis.push(PhiNode {
                                    dst: *dst,
                                    sources: phi_srcs,
                                });
                            }
                        }
                    } else {
                        let dst = reg_map.get(*param_reg);
                        let phi_srcs: Vec<(BlockId, VReg)> = sources
                            .iter()
                            .filter(|(_, idx, src_chunk_idx, _)| {
                                *idx == param_idx && *src_chunk_idx == 0
                            })
                            .map(|(pred, _, _, vreg)| (*pred, *vreg))
                            .collect();
                        if !phi_srcs.is_empty() {
                            mblock.phis.push(PhiNode {
                                dst,
                                sources: phi_srcs,
                            });
                        }
                    }

                    // 4-state: add mask phi node
                    if four_state {
                        if let Some(m_sources) = mask_phi_sources.get(&mblock.id) {
                            if let Some(mask_chunks) = saved_wide_masks.get(param_reg) {
                                for (chunk_idx, (mask_dst, _)) in mask_chunks.iter().enumerate() {
                                    let mask_phi_srcs: Vec<(BlockId, VReg)> = m_sources
                                        .iter()
                                        .filter(|(_, idx, src_chunk_idx, _)| {
                                            *idx == param_idx && *src_chunk_idx == chunk_idx
                                        })
                                        .map(|(pred, _, _, vreg)| (*pred, *vreg))
                                        .collect();
                                    if !mask_phi_srcs.is_empty() {
                                        mblock.phis.push(PhiNode {
                                            dst: *mask_dst,
                                            sources: mask_phi_srcs,
                                        });
                                    }
                                }
                            } else if let Some(mask_dst) =
                                saved_mask_map.map.get(param_reg.0).copied().flatten()
                            {
                                let mask_phi_srcs: Vec<(BlockId, VReg)> = m_sources
                                    .iter()
                                    .filter(|(_, idx, src_chunk_idx, _)| {
                                        *idx == param_idx && *src_chunk_idx == 0
                                    })
                                    .map(|(pred, _, _, vreg)| (*pred, *vreg))
                                    .collect();
                                if !mask_phi_srcs.is_empty() {
                                    mblock.phis.push(PhiNode {
                                        dst: mask_dst,
                                        sources: mask_phi_srcs,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Update spill_descs to match final vreg count
    while func.spill_descs.len() < func.vregs.count() as usize {
        func.spill_descs.push(SpillDesc::transient());
    }

    func
}

/// Compute a bitmask of `width` bits (e.g., width=8 → 0xFF).
/// Returns u64::MAX for width >= 64 to avoid shift overflow.
#[inline]
fn mask_for_width(width: usize) -> u64 {
    if width >= 64 {
        u64::MAX
    } else {
        (1u64 << width) - 1
    }
}

/// Wide value representations owned by ISel.
///
/// Insertion is intentionally private to `ISelContext::set_wide_chunks`; all
/// other users can only query or remove a representation.  This keeps the
/// declared SIR width and the native representation synchronized.
#[derive(Default)]
struct WideRegMap {
    chunks: crate::HashMap<RegisterId, Vec<(VReg, usize)>>,
}

impl WideRegMap {
    fn get(&self, reg: &RegisterId) -> Option<&Vec<(VReg, usize)>> {
        self.chunks.get(reg)
    }

    fn contains_key(&self, reg: &RegisterId) -> bool {
        self.chunks.contains_key(reg)
    }

    fn remove(&mut self, reg: &RegisterId) -> Option<Vec<(VReg, usize)>> {
        self.chunks.remove(reg)
    }

    fn iter(&self) -> impl Iterator<Item = (&RegisterId, &Vec<(VReg, usize)>)> {
        self.chunks.iter()
    }

    fn replace(&mut self, reg: RegisterId, chunks: Vec<(VReg, usize)>) {
        self.chunks.insert(reg, chunks);
    }
}

type WideMaskMap = crate::HashMap<RegisterId, Vec<(VReg, usize)>>;

/// Tracks known constant values for SIR registers (for constant folding in ISel).
type ConstMap = crate::HashMap<RegisterId, u64>;

struct ISelContext<'a> {
    vregs: &'a mut VRegAllocator,
    spill_descs: &'a mut Vec<SpillDesc>,
    reg_map: &'a mut RegMap,
    register_types: &'a crate::HashMap<RegisterId, RegisterType>,
    layout: &'a MemoryLayout,
    wide_regs: WideRegMap,
    /// Known constant values for SIR registers (from Imm, Mul of constants, etc.)
    consts: ConstMap,
    /// RegisterId → (sim-state address, static load bit offset).
    /// Used by Slice to reload memory after an intervening partial Store.
    reg_addrs: crate::HashMap<RegisterId, (RegionedAbsoluteAddr, usize)>,
    /// Conservative lower bound for the number of low zero bits in a SIR value.
    /// This lets dynamic bit offsets that are known byte-aligned use indexed
    /// byte addressing without a dynamic intra-byte shift.
    low_zero_bits: crate::HashMap<RegisterId, u32>,
    /// Whether 4-state simulation is enabled.
    four_state: bool,
    /// Maps SIR RegisterId → mask VReg (parallel to reg_map).
    mask_map: RegMap,
    /// Known effective bit width per VReg. If a VReg is known to have at most
    /// `w` significant bits (upper bits guaranteed zero), AND masking to `w`
    /// bits can be elided. Populated by Load (movzx), Cmp (0/1), AndImm, etc.
    known_bits: crate::HashMap<VReg, usize>,
    /// Wide mask chunks (parallel to wide_regs).
    wide_masks: WideMaskMap,
    /// Width=0 trigger-only stores do not write memory. Within one MIR block,
    /// rechecking the same physical byte for the same trigger id is redundant
    /// until a real Store/Commit may change memory.
    trigger_only_seen: HashSet<(i32, usize)>,
    /// Present only when this function has a final commit run covering every
    /// sparse region it can write.
    sparse_descriptor_table: Option<ConstantTableId>,
    trace_regs: HashSet<RegisterId>,
}

impl<'a> ISelContext<'a> {
    /// Allocate a fresh VReg with the given spill descriptor.
    fn alloc_vreg(&mut self, desc: SpillDesc) -> VReg {
        let vreg = self.vregs.alloc();
        // Grow spill_descs if needed
        while self.spill_descs.len() <= vreg.0 as usize {
            self.spill_descs.push(SpillDesc::transient());
        }
        self.spill_descs[vreg.0 as usize] = desc;
        vreg
    }

    /// Get the bit width of a SIR register.
    fn sir_width(&self, reg: &RegisterId) -> usize {
        self.register_types[reg].width()
    }

    /// Get the mask VReg for a SIR register (zero constant if not 4-state).
    fn get_mask(&mut self, reg: RegisterId, block: &mut MBlock) -> VReg {
        if self.four_state {
            self.mask_map.map[reg.0].unwrap_or_else(|| {
                // Not yet defined — return zero
                let z = self.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                z
            })
        } else {
            let z = self.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            z
        }
    }

    /// Set the mask VReg for a SIR register.
    fn set_mask(&mut self, reg: RegisterId, vreg: VReg) {
        if self.four_state {
            self.mask_map.set(reg, vreg);
        }
    }

    fn const_mask_value(&self, reg: RegisterId) -> Option<u64> {
        if !self.four_state {
            return Some(0);
        }

        let vreg = self.mask_map.map.get(reg.0).copied().flatten()?;
        match self.spill_descs.get(vreg.0 as usize).map(|desc| &desc.kind) {
            Some(SpillKind::Remat { value }) => Some(*value),
            _ => None,
        }
    }

    /// Resolve the mask byte offset for a variable.
    /// The mask is stored immediately after the value in memory.
    fn mask_byte_offset(&self, addr: &RegionedAbsoluteAddr, bit_offset: usize) -> i32 {
        let abs_addr = addr.absolute_addr();
        self.byte_offset(addr, bit_offset) + self.layout.plane_size(&abs_addr) as i32
    }

    /// Whether the given address refers to a 4-state variable.
    fn is_4state_var(&self, addr: &RegionedAbsoluteAddr) -> bool {
        self.four_state
            && self
                .layout
                .is_4states
                .get(&addr.absolute_addr())
                .copied()
                .unwrap_or(false)
    }

    /// Resolve byte offset for a regioned address + bit offset.
    fn byte_offset(&self, addr: &RegionedAbsoluteAddr, bit_offset: usize) -> i32 {
        self.static_byte_and_intra(addr, bit_offset).0
    }

    fn static_byte_and_intra(
        &self,
        addr: &RegionedAbsoluteAddr,
        bit_offset: usize,
    ) -> (i32, usize) {
        self.layout
            .regioned_static_byte_and_intra(addr, bit_offset)
            .expect("native static state offset must fit i32")
    }

    /// Choose OpSize for a given bit width, clamping to the smallest
    /// native size that fits.
    fn op_size_for_width(width_bits: usize) -> OpSize {
        match width_bits {
            0..=8 => OpSize::S8,
            9..=16 => OpSize::S16,
            17..=32 => OpSize::S32,
            _ => OpSize::S64,
        }
    }

    /// Return the smallest native access size only when it covers exactly the
    /// bytes allocated for the logical value. A wider access would read or
    /// write an adjacent packed variable.
    fn exact_storage_access_size(width_bits: usize) -> Option<OpSize> {
        if width_bits == 0 || width_bits > 64 {
            return None;
        }
        let size = Self::op_size_for_width(width_bits);
        (size.bytes() as usize == width_bits.div_ceil(8)).then_some(size)
    }

    fn full_static_access_size(
        &self,
        addr: &RegionedAbsoluteAddr,
        bit_offset: usize,
        width_bits: usize,
    ) -> Option<OpSize> {
        if let Some(array) = self.layout.unpacked_arrays.get(&addr.absolute_addr()) {
            if width_bits == array.element_width && bit_offset.is_multiple_of(array.element_width) {
                return Self::exact_storage_access_size(width_bits);
            }
            // A complete unpacked array is not one contiguous scalar object
            // in element-strided mode. It must be gathered/scattered element
            // by element even when its logical width is 8/16/32/64 bits.
            return None;
        }
        if bit_offset != 0 {
            return None;
        }
        let var_width = self.layout.widths.get(&addr.absolute_addr()).copied()?;
        (var_width == width_bits)
            .then(|| Self::exact_storage_access_size(width_bits))
            .flatten()
    }

    fn full_static_store_size(
        &self,
        addr: &RegionedAbsoluteAddr,
        bit_offset: usize,
        width_bits: usize,
    ) -> Option<OpSize> {
        self.full_static_access_size(addr, bit_offset, width_bits)
    }

    fn full_static_load_size(
        &self,
        addr: &RegionedAbsoluteAddr,
        bit_offset: usize,
        width_bits: usize,
    ) -> Option<OpSize> {
        self.full_static_access_size(addr, bit_offset, width_bits)
    }

    fn access_size_has_padding(size: OpSize, width_bits: usize) -> bool {
        size.bytes() as usize * 8 != width_bits
    }

    /// A whole dynamically indexed array element occupies one independently
    /// padded native scalar slot. Accessing that slot directly is legal when
    /// the offset contains no additional bit displacement.
    fn full_element_access_size(
        &self,
        addr: &RegionedAbsoluteAddr,
        offset: &SIROffset,
        width_bits: usize,
    ) -> Option<OpSize> {
        let array = self.layout.unpacked_arrays.get(&addr.absolute_addr())?;
        if width_bits != array.element_width {
            return None;
        }
        match offset {
            SIROffset::Element {
                element_width,
                bit_offset: 0,
                dynamic_bit_offset: None,
                ..
            } if *element_width == array.element_width => {
                Self::exact_storage_access_size(width_bits)
            }
            _ => None,
        }
    }

    fn mask_for_store_width(&mut self, block: &mut MBlock, src: VReg, width_bits: usize) -> VReg {
        if width_bits >= 64
            || self
                .known_bits
                .get(&src)
                .is_some_and(|&known_bits| known_bits <= width_bits)
        {
            return src;
        }
        let masked = self.alloc_vreg(SpillDesc::transient());
        self.emit_and_imm(block, masked, src, mask_for_width(width_bits));
        masked
    }

    /// Emit AND with immediate, handling 64-bit values that don't fit i32.
    /// Elides the AND entirely if the source is already known to fit within
    /// the mask (redundant mask elimination).
    fn emit_and_imm(&mut self, block: &mut MBlock, dst: VReg, src: VReg, imm: u64) {
        let signed = imm as i64;
        if imm == u64::MAX {
            // AND with all-ones is identity
            if dst != src {
                self.emit_mov(block, dst, src);
            }
            return;
        }

        // Check if src is already known to fit within the mask.
        // mask_for_width(w) = (1 << w) - 1. If src's known_bits <= w,
        // the AND is redundant.
        if let Some(&src_bits) = self.known_bits.get(&src) {
            // imm = mask_for_width(w) means all bits above w are 0.
            // If src_bits <= w, src already has zeros above w.
            let mask_width = 64 - imm.leading_zeros() as usize; // bits needed to represent imm
            if imm == mask_for_width(mask_width) && src_bits <= mask_width {
                // Redundant AND: src is already within mask
                if dst != src {
                    self.emit_mov(block, dst, src);
                }
                return;
            }
        }

        // Track output known bits
        let out_bits = 64 - imm.leading_zeros() as usize;
        if imm == mask_for_width(out_bits) {
            self.known_bits.insert(dst, out_bits);
        }

        if imm <= u32::MAX as u64 {
            block.push(MInst::AndImm32 {
                dst,
                src,
                imm: imm as u32,
            });
        } else if signed >= i32::MIN as i64 && signed <= i32::MAX as i64 {
            block.push(MInst::AndImm { dst, src, imm });
        } else {
            // 64-bit immediate: decompose into LoadImm + And
            let tmp = self.alloc_vreg(SpillDesc::remat(imm));
            block.push(MInst::LoadImm {
                dst: tmp,
                value: imm,
            });
            block.push(MInst::And {
                dst,
                lhs: src,
                rhs: tmp,
            });
        }
    }

    fn emit_mov(&mut self, block: &mut MBlock, dst: VReg, src: VReg) {
        if dst == src {
            return;
        }
        let narrow32 = self.known_bits.get(&src).is_some_and(|&bits| bits <= 32);
        if narrow32 {
            block.push(MInst::Mov32 { dst, src });
        } else {
            block.push(MInst::Mov { dst, src });
        }
        if let Some(desc) = self.spill_descs.get(src.0 as usize).cloned() {
            self.spill_descs[dst.0 as usize] = desc.copy_for_snapshot();
        }
        if let Some(bits) = self.known_bits.get(&src).copied() {
            self.known_bits.insert(dst, bits);
        } else {
            self.known_bits.remove(&dst);
        }
    }

    fn emit_alias_mov(&mut self, block: &mut MBlock, dst: VReg, src: VReg) {
        if dst == src {
            return;
        }
        let narrow32 = self.known_bits.get(&src).is_some_and(|&bits| bits <= 32);
        if narrow32 {
            block.push(MInst::Mov32 { dst, src });
        } else {
            block.push(MInst::Mov { dst, src });
        }
        if let Some(desc) = self.spill_descs.get(src.0 as usize).cloned() {
            self.spill_descs[dst.0 as usize] = match desc.kind {
                SpillKind::SimState {
                    addr,
                    bit_offset,
                    width_bits,
                } => SpillDesc::sim_state_alias(addr, bit_offset, width_bits, desc.spill_cost == 0),
                _ => desc,
            };
        }
        if let Some(bits) = self.known_bits.get(&src).copied() {
            self.known_bits.insert(dst, bits);
        } else {
            self.known_bits.remove(&dst);
        }
    }

    /// Emit bitfield insert: dst = (base_word & ~(mask << shift)) | ((val & mask) << shift)
    /// Decomposes into basic ALU ops (no pseudo-instruction).
    fn emit_bfi(
        &mut self,
        block: &mut MBlock,
        dst: VReg,
        base_word: VReg,
        val: VReg,
        shift: u8,
        mask: u64,
    ) {
        let clear_mask = !(mask << shift);
        // cleared = base_word & clear_mask
        let cleared = self.alloc_vreg(SpillDesc::transient());
        self.emit_and_imm(block, cleared, base_word, clear_mask);
        // masked_val = val & mask
        let masked_val = self.alloc_vreg(SpillDesc::transient());
        if mask != u64::MAX {
            self.emit_and_imm(block, masked_val, val, mask);
        } else {
            self.emit_mov(block, masked_val, val);
        }
        // shifted_val = masked_val << shift
        if shift > 0 {
            let shifted = self.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShlImm {
                dst: shifted,
                src: masked_val,
                imm: shift,
            });
            block.push(MInst::Or {
                dst,
                lhs: cleared,
                rhs: shifted,
            });
        } else {
            block.push(MInst::Or {
                dst,
                lhs: cleared,
                rhs: masked_val,
            });
        }
    }

    /// Number of 64-bit chunks needed for a given bit width.
    fn num_chunks(width_bits: usize) -> usize {
        width_bits.div_ceil(64)
    }

    /// Get or create wide chunks for a SIR register.
    /// If the register is already tracked as wide, returns existing chunks.
    /// If it's a scalar (≤64-bit), promotes it to a wide value with zero-extended chunks.
    fn get_wide_chunks(&mut self, reg: &RegisterId, block: &mut MBlock) -> Vec<(VReg, usize)> {
        if let Some(chunks) = self.wide_regs.get(reg) {
            return chunks.clone();
        }
        // Scalar register: promote to wide by putting it in chunk 0, zeros elsewhere
        let vreg = self.reg_map.get(*reg);
        let width = self.sir_width(reg);
        let n_chunks = Self::num_chunks(width);
        let mut chunks = Vec::with_capacity(n_chunks);
        let chunk0_width = width.min(64);
        chunks.push((vreg, chunk0_width));
        for _ in 1..n_chunks {
            let zero = self.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            chunks.push((zero, 64));
        }
        chunks
    }

    /// Store chunks for a SIR register.
    ///
    /// A register whose declared SIR width fits in one machine word must stay
    /// scalar even when it was produced by an operation with wide operands.
    /// Keeping a one-chunk entry in `wide_regs` makes later width-sensitive
    /// operations (notably arithmetic shifts) use the 64-bit wide lowering
    /// instead of the register's declared width.
    fn set_wide_chunks(&mut self, reg: RegisterId, mut chunks: Vec<(VReg, usize)>) {
        let width = self.sir_width(&reg);
        let expected_chunks = Self::num_chunks(width).max(1);
        chunks.truncate(expected_chunks);
        for (index, (_, chunk_width)) in chunks.iter_mut().enumerate() {
            *chunk_width = width.saturating_sub(index * 64).min(64);
        }
        if let Some(&(chunk0, _)) = chunks.first() {
            // Keep the scalar slot pointing at chunk 0 so block args, stores,
            // and other narrow consumers still see a defined VReg.
            self.reg_map.set(reg, chunk0);
        }
        if width <= 64 {
            self.wide_regs.remove(&reg);
        } else {
            self.wide_regs.replace(reg, chunks);
        }
    }

    /// Materialize the declared SIR width after an operation with wide inputs
    /// produced a scalar result.
    ///
    /// `set_wide_chunks` keeps chunk zero in `reg_map` for narrow destinations,
    /// but that machine word may still contain bits above the destination's SIR
    /// width.  Consumers are allowed to trust `known_bits`, so merely recording
    /// the narrow width would make those upper bits observable when a later mask
    /// is eliminated.
    fn canonicalize_narrow_wide_result(&mut self, block: &mut MBlock, reg: RegisterId) {
        let width = self.sir_width(&reg);
        if width >= 64 {
            return;
        }

        let raw = self.reg_map.get(reg);
        let canonical = self.alloc_vreg(SpillDesc::transient());
        let mask = mask_for_width(width);
        if mask <= u32::MAX as u64 {
            block.push(MInst::AndImm32 {
                dst: canonical,
                src: raw,
                imm: mask as u32,
            });
        } else {
            let mask_reg = self.alloc_vreg(SpillDesc::remat(mask));
            block.push(MInst::LoadImm {
                dst: mask_reg,
                value: mask,
            });
            block.push(MInst::And {
                dst: canonical,
                lhs: raw,
                rhs: mask_reg,
            });
        }
        self.reg_map.set(reg, canonical);
        self.known_bits.insert(canonical, width);
    }

    /// Check the representation invariant for value chunks.
    ///
    /// `wide_regs` is deliberately a value-only map: every entry must have a
    /// real SIR type wider than one machine word.  Narrow results of wide
    /// operations are kept in `reg_map` and must never be allowed to re-enter
    /// this map through a direct insertion.
    fn verify_wide_values(&self) {
        for (reg, chunks) in self.wide_regs.iter() {
            let width = self.sir_width(reg);
            assert!(
                width > 64,
                "narrow SIR register r{} has a wide native representation (width={width})",
                reg.0
            );
            let expected_chunks = Self::num_chunks(width);
            assert_eq!(
                chunks.len(),
                expected_chunks,
                "wide SIR register r{} has {} chunks, expected {expected_chunks}",
                reg.0,
                chunks.len()
            );
            for (index, (_, chunk_width)) in chunks.iter().enumerate() {
                let expected_width = width.saturating_sub(index * 64).min(64);
                assert_eq!(
                    *chunk_width, expected_width,
                    "wide SIR register r{} chunk {index} has width {chunk_width}, expected {expected_width}",
                    reg.0
                );
            }
        }
    }

    /// Get chunk `i` from a wide value, or emit a zero constant if missing.
    fn wide_chunk_or_zero(
        &mut self,
        chunks: &[(VReg, usize)],
        i: usize,
        block: &mut MBlock,
    ) -> VReg {
        chunks.get(i).map(|c| c.0).unwrap_or_else(|| {
            let z = self.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            z
        })
    }
}

fn low_zero_bits_const(value: u64) -> u32 {
    if value == 0 {
        64
    } else {
        value.trailing_zeros()
    }
}

fn low_zero_bits_reg(ctx: &ISelContext<'_>, reg: RegisterId) -> u32 {
    ctx.consts
        .get(&reg)
        .copied()
        .map(low_zero_bits_const)
        .unwrap_or_else(|| ctx.low_zero_bits.get(&reg).copied().unwrap_or(0))
}

fn set_low_zero_bits(ctx: &mut ISelContext<'_>, reg: RegisterId, bits: u32) {
    ctx.low_zero_bits.insert(reg, bits.min(64));
}

fn lower_bool_value(ctx: &mut ISelContext, block: &mut MBlock, src: VReg) -> VReg {
    if ctx.known_bits.get(&src).is_some_and(|&bits| bits <= 1) {
        return src;
    }

    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let dst = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst,
        lhs: src,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    ctx.known_bits.insert(dst, 1);
    dst
}

fn lower_low_bit(ctx: &mut ISelContext, block: &mut MBlock, src: VReg) -> VReg {
    if ctx.known_bits.get(&src).is_some_and(|&bits| bits <= 1) {
        return src;
    }

    let dst = ctx.alloc_vreg(SpillDesc::transient());
    ctx.emit_and_imm(block, dst, src, 1);
    dst
}

#[derive(Default)]
struct PriorityEncodePlans {
    roots: HashMap<usize, PriorityEncodePlan>,
    skip_indices: HashSet<usize>,
}

#[derive(Clone)]
struct PriorityEncodePlan {
    root_idx: usize,
    dst: RegisterId,
    src: RegisterId,
    width: usize,
}

#[derive(Default)]
struct SelectorBranchTablePlans {
    roots: HashMap<crate::BlockId, SelectorBranchTablePlan>,
    removed_blocks: HashSet<crate::BlockId>,
}

struct SelectorBranchTablePlan {
    selector: RegisterId,
    selector_width: usize,
    targets: Box<[crate::BlockId]>,
    skip_indices: HashSet<usize>,
}

struct DenseBranchCondition {
    selector: RegisterId,
    selector_width: usize,
    key: u64,
    covered_indices: HashSet<usize>,
}

fn match_dense_branch_condition(
    block: &BasicBlock<RegionedAbsoluteAddr>,
    register_types: &HashMap<RegisterId, RegisterType>,
    constants: &HashMap<RegisterId, ExactSirConstant>,
    uses: &HashMap<RegisterId, Vec<SirUseSite>>,
    condition: RegisterId,
) -> Option<DenseBranchCondition> {
    let definitions = collect_sir_defs(block);
    let mut cursor = condition;
    let mut covered_indices = HashSet::default();
    loop {
        let &index = definitions.get(&cursor)?;
        match &block.instructions[index] {
            SIRInstruction::Unary(
                _,
                UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Or,
                source,
            ) => {
                covered_indices.insert(index);
                cursor = *source;
            }
            _ => break,
        }
    }

    let &compare_index = definitions.get(&cursor)?;
    let SIRInstruction::Binary(_, lhs, operation, rhs) = &block.instructions[compare_index] else {
        return None;
    };
    let (selector, key_register, key) = match operation {
        BinaryOp::EqWildcard => (*lhs, *rhs, constants.get(rhs)?.value),
        BinaryOp::Eq => match (constants.get(lhs), constants.get(rhs)) {
            (None, Some(key)) => (*lhs, *rhs, key.value),
            (Some(key), None) => (*rhs, *lhs, key.value),
            _ => return None,
        },
        _ => return None,
    };
    let selector_width = register_types.get(&selector)?.width();
    if selector_width == 0 || selector_width > 8 || key & !mask_for_width(selector_width) != 0 {
        return None;
    }
    covered_indices.insert(compare_index);
    if let Some(&key_index) = definitions.get(&key_register) {
        covered_indices.insert(key_index);
    }

    for &index in &covered_indices {
        let definition = sir_def_reg(&block.instructions[index])?;
        if uses.get(&definition).is_some_and(|sites| {
            sites.iter().any(|site| {
                site.block != block.id
                    || site
                        .inst_idx
                        .is_some_and(|use_index| !covered_indices.contains(&use_index))
                    || (site.inst_idx.is_none() && definition != condition)
            })
        }) {
            return None;
        }
    }

    Some(DenseBranchCondition {
        selector,
        selector_width,
        key,
        covered_indices,
    })
}

fn find_selector_branch_table_plans(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    constants: &HashMap<RegisterId, ExactSirConstant>,
    uses: &HashMap<RegisterId, Vec<SirUseSite>>,
) -> SelectorBranchTablePlans {
    let mut predecessors: HashMap<crate::BlockId, Vec<crate::BlockId>> =
        eu.blocks.keys().map(|&block| (block, Vec::new())).collect();
    for block in eu.blocks.values() {
        let successors = match &block.terminator {
            SIRTerminator::Jump(target, _) => vec![*target],
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => vec![true_block.0, false_block.0],
            SIRTerminator::Switch { cases, default, .. } => cases
                .iter()
                .map(|case| case.target)
                .chain(std::iter::once(*default))
                .collect(),
            SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
        };
        for successor in successors {
            predecessors.entry(successor).or_default().push(block.id);
        }
    }

    let mut result = SelectorBranchTablePlans::default();
    for root in ordered_sir_blocks(eu) {
        if result.removed_blocks.contains(&root) {
            continue;
        }
        let mut current = root;
        let mut selector = None;
        let mut selector_width = None;
        let mut targets = Vec::<Option<crate::BlockId>>::new();
        let mut decision_blocks = Vec::new();
        let mut root_skip = HashSet::default();
        let mut default = None;
        let mut valid = true;

        loop {
            let block = &eu.blocks[&current];
            let SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } = &block.terminator
            else {
                if selector.is_some() {
                    default = Some(current);
                } else {
                    valid = false;
                }
                break;
            };
            if !true_block.1.is_empty()
                || !false_block.1.is_empty()
                || !eu.blocks[&true_block.0].params.is_empty()
            {
                if selector.is_some() {
                    default = Some(current);
                } else {
                    valid = false;
                }
                break;
            }
            let Some(condition) =
                match_dense_branch_condition(block, &eu.register_map, constants, uses, *cond)
            else {
                if selector.is_some() {
                    default = Some(current);
                } else {
                    valid = false;
                }
                break;
            };
            if let Some(expected) = selector {
                if expected != condition.selector
                    || selector_width != Some(condition.selector_width)
                    || block
                        .instructions
                        .iter()
                        .enumerate()
                        .any(|(index, _)| !condition.covered_indices.contains(&index))
                {
                    default = Some(current);
                    break;
                }
            } else {
                selector = Some(condition.selector);
                selector_width = Some(condition.selector_width);
                targets.resize(1usize << condition.selector_width, None);
                root_skip = condition.covered_indices.clone();
            }
            let key = condition.key as usize;
            if targets[key].is_some() {
                default = Some(current);
                break;
            }
            targets[key] = Some(true_block.0);
            decision_blocks.push(current);
            if targets.iter().all(Option::is_some) {
                default = targets[0];
                break;
            }

            let next = false_block.0;
            if next == root
                || !eu.blocks[&next].params.is_empty()
                || predecessors.get(&next).map(Vec::as_slice) != Some([current].as_slice())
            {
                default = Some(next);
                break;
            }
            current = next;
        }

        let case_count = targets.iter().filter(|target| target.is_some()).count();
        if !valid
            || case_count < 4
            || case_count.saturating_mul(8) < targets.len()
            || default.is_none()
        {
            continue;
        }
        let default = default.expect("accepted selector dispatch has a default");
        let targets = targets
            .into_iter()
            .map(|target| target.unwrap_or(default))
            .collect::<Vec<_>>();
        let target_blocks = targets.iter().copied().collect::<HashSet<_>>();
        if decision_blocks
            .iter()
            .skip(1)
            .any(|block| target_blocks.contains(block))
        {
            continue;
        }
        result
            .removed_blocks
            .extend(decision_blocks.iter().skip(1).copied());
        result.roots.insert(
            root,
            SelectorBranchTablePlan {
                selector: selector.expect("valid branch table has a selector"),
                selector_width: selector_width.expect("valid branch table has a width"),
                targets: targets.into(),
                skip_indices: root_skip,
            },
        );
    }

    let mut reachable = HashSet::default();
    let mut worklist = vec![eu.entry_block_id];
    while let Some(block_id) = worklist.pop() {
        if !reachable.insert(block_id) {
            continue;
        }
        if let Some(plan) = result.roots.get(&block_id) {
            worklist.extend(plan.targets.iter().copied());
            continue;
        }
        let block = &eu.blocks[&block_id];
        match &block.terminator {
            SIRTerminator::Jump(target, _) => worklist.push(*target),
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                worklist.push(true_block.0);
                worklist.push(false_block.0);
            }
            SIRTerminator::Switch { cases, default, .. } => {
                worklist.extend(cases.iter().map(|case| case.target));
                worklist.push(*default);
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    result
        .removed_blocks
        .extend(eu.blocks.keys().filter(|block| !reachable.contains(block)));
    result
}

#[derive(Default)]
struct DenseLookupPlans {
    roots: HashMap<usize, DenseLookupPlan>,
    skip_indices: HashSet<usize>,
}

#[derive(Clone, Debug)]
struct DenseLookupPlan {
    root_idx: usize,
    dst: RegisterId,
    selector: RegisterId,
    selector_width: usize,
    default: RegisterId,
    entries: Vec<u64>,
    table: Option<ConstantTableId>,
}

struct DenseLookupCandidate {
    plan: DenseLookupPlan,
    covered_indices: HashSet<usize>,
}

#[derive(Default)]
struct DenseLookupEmitCache {
    byte_indices: HashMap<(RegisterId, usize), VReg>,
    table_addrs: HashMap<ConstantTableId, VReg>,
}

fn find_dense_lookup_plans(
    block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    register_types: &HashMap<RegisterId, RegisterType>,
    constants: &HashMap<RegisterId, ExactSirConstant>,
    uses: &HashMap<RegisterId, Vec<SirUseSite>>,
) -> DenseLookupPlans {
    let defs = collect_sir_defs(block);
    let mut candidates = Vec::new();
    for (root_idx, inst) in block.instructions.iter().enumerate() {
        let SIRInstruction::Mux(root_dst, ..) = inst else {
            continue;
        };
        let only_feeds_later_chain_stages = uses.get(root_dst).is_some_and(|sites| {
            !sites.is_empty()
                && sites.iter().all(|site| {
                    site.block == block.id
                        && site.inst_idx.is_some_and(|use_idx| {
                            matches!(
                                block.instructions.get(use_idx),
                                Some(SIRInstruction::Mux(_, _, _, else_value))
                                    if else_value == root_dst
                            )
                        })
                })
        });
        if only_feeds_later_chain_stages {
            continue;
        }
        if let Some(candidate) = collect_dense_lookup_candidate(
            block,
            register_types,
            constants,
            &defs,
            root_idx,
            *root_dst,
        ) {
            candidates.push(candidate);
        }
    }
    if candidates.is_empty() {
        return DenseLookupPlans::default();
    }

    let mut covered_indices = HashSet::default();
    let mut root_indices = HashSet::default();
    let mut roots = HashMap::default();
    for candidate in candidates {
        root_indices.insert(candidate.plan.root_idx);
        covered_indices.extend(candidate.covered_indices);
        roots.insert(candidate.plan.root_idx, candidate.plan);
    }

    // Compute the greatest removable subset of the covered union.  Roots are
    // replaced in-place and therefore remain removable even though their
    // values have users outside the union.  Any other covered definition with
    // an outside user is retained, then retention is propagated backwards to
    // its covered operands.  This is what permits several lookup roots to
    // share comparison/constant definitions without leaving those definitions
    // behind merely because another recognized root also uses them.
    let mut retained = HashSet::default();
    let mut worklist = Vec::new();
    for &idx in &covered_indices {
        if root_indices.contains(&idx) {
            continue;
        }
        let Some(def) = sir_def_reg(&block.instructions[idx]) else {
            continue;
        };
        let has_outside_use = uses.get(&def).is_some_and(|sites| {
            sites.iter().any(|site| {
                site.block != block.id
                    || site
                        .inst_idx
                        .is_none_or(|use_idx| !covered_indices.contains(&use_idx))
            })
        });
        if has_outside_use && retained.insert(idx) {
            worklist.push(idx);
        }
    }
    while let Some(idx) = worklist.pop() {
        collect_sir_inst_uses(&block.instructions[idx], |operand| {
            let Some(&operand_idx) = defs.get(&operand) else {
                return;
            };
            if covered_indices.contains(&operand_idx)
                && !root_indices.contains(&operand_idx)
                && retained.insert(operand_idx)
            {
                worklist.push(operand_idx);
            }
        });
    }

    let skip_indices = covered_indices
        .into_iter()
        .filter(|idx| !retained.contains(idx))
        .collect();
    DenseLookupPlans {
        roots,
        skip_indices,
    }
}

fn collect_dense_lookup_candidate(
    block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    register_types: &HashMap<RegisterId, RegisterType>,
    constants: &HashMap<RegisterId, ExactSirConstant>,
    defs: &HashMap<RegisterId, usize>,
    root_idx: usize,
    root_dst: RegisterId,
) -> Option<DenseLookupCandidate> {
    let result_width = register_types.get(&root_dst)?.width();
    if result_width == 0 || result_width > 64 {
        return None;
    }

    let mut cursor = root_dst;
    let mut selector = None;
    let mut selector_width = None;
    let mut items = Vec::new();
    let mut keys = HashSet::default();
    let mut covered_indices = HashSet::default();

    let default = loop {
        let &mux_idx = defs.get(&cursor)?;
        let SIRInstruction::Mux(dst, cond, then_value, else_value) = &block.instructions[mux_idx]
        else {
            return None;
        };
        if *dst != cursor
            || register_types.get(dst)?.width() != result_width
            || register_types.get(then_value)?.width() != result_width
            || register_types.get(else_value)?.width() != result_width
        {
            return None;
        }

        let matched = match_dense_lookup_condition(block, register_types, constants, defs, *cond)?;
        if let Some(expected) = selector {
            if expected != matched.selector {
                return None;
            }
        } else {
            selector = Some(matched.selector);
            selector_width = Some(matched.selector_width);
        }
        if !keys.insert(matched.key) {
            // Duplicate exact keys make mux priority observable.  Do not
            // silently choose either occurrence when constructing the table.
            return None;
        }

        let then_constant = constants.get(then_value)?;
        let table_value = then_constant.value & mask_for_width(result_width);
        items.push((matched.key, table_value));
        covered_indices.insert(mux_idx);
        covered_indices.extend(matched.covered_indices);
        if let Some(&idx) = defs.get(then_value) {
            covered_indices.insert(idx);
        }

        if let Some(&previous_idx) = defs.get(else_value)
            && matches!(block.instructions[previous_idx], SIRInstruction::Mux(..))
        {
            cursor = *else_value;
            continue;
        }
        if let Some(&idx) = defs.get(else_value) {
            covered_indices.insert(idx);
        }
        break *else_value;
    };

    let selector = selector?;
    let selector_width = selector_width?;
    if selector_width == 0 || selector_width >= usize::BITS as usize {
        return None;
    }
    let domain_size = 1usize.checked_shl(selector_width as u32)?;
    if items.len() != domain_size {
        return None;
    }
    // A full two-case chain already lowers to roughly the same four MIR
    // operations as address-mask, scale, table-address, and load.  Require a
    // strict instruction-count win; domain sizes are powers of two, so the
    // next profitable shape has four cases.
    if domain_size < 4 {
        return None;
    }

    // Allocate only after proving that the already-existing chain contains
    // exactly one stage for every selector value.
    let mut entries = vec![0u64; items.len()];
    let mut occupied = vec![false; items.len()];
    for (key, value) in items {
        let index = usize::try_from(key).ok()?;
        if index >= entries.len() || occupied[index] {
            return None;
        }
        entries[index] = value;
        occupied[index] = true;
    }
    if occupied.iter().any(|occupied| !occupied) {
        return None;
    }

    Some(DenseLookupCandidate {
        plan: DenseLookupPlan {
            root_idx,
            dst: root_dst,
            selector,
            selector_width,
            default,
            entries,
            table: None,
        },
        covered_indices,
    })
}

struct DenseLookupCondition {
    selector: RegisterId,
    selector_width: usize,
    key: u64,
    covered_indices: HashSet<usize>,
}

fn match_dense_lookup_condition(
    block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    register_types: &HashMap<RegisterId, RegisterType>,
    constants: &HashMap<RegisterId, ExactSirConstant>,
    defs: &HashMap<RegisterId, usize>,
    cond: RegisterId,
) -> Option<DenseLookupCondition> {
    let mut cursor = cond;
    let mut covered_indices = HashSet::default();
    while let Some(&idx) = defs.get(&cursor) {
        match &block.instructions[idx] {
            SIRInstruction::Unary(_, UnaryOp::Ident, inner) => {
                covered_indices.insert(idx);
                cursor = *inner;
            }
            SIRInstruction::Concat(_, args) if !args.is_empty() => {
                let (&inner, high) = args.split_last()?;
                if register_types.get(&inner)?.width() != 1 {
                    return None;
                }
                for high_reg in high {
                    if constants.get(high_reg)?.value != 0 {
                        return None;
                    }
                    if let Some(&constant_idx) = defs.get(high_reg) {
                        covered_indices.insert(constant_idx);
                    }
                }
                covered_indices.insert(idx);
                cursor = inner;
            }
            _ => break,
        }
    }

    let &compare_idx = defs.get(&cursor)?;
    let SIRInstruction::Binary(_, lhs, op @ (BinaryOp::Eq | BinaryOp::EqWildcard), rhs) =
        &block.instructions[compare_idx]
    else {
        return None;
    };
    let (selector, key_reg, key) = match op {
        BinaryOp::EqWildcard => {
            // IEEE wildcard matching is directional.  Only a definite RHS
            // immediate is an exact lookup key.
            let key = constants.get(rhs)?.value;
            if constants.contains_key(lhs) {
                return None;
            }
            (*lhs, *rhs, key)
        }
        BinaryOp::Eq => match (constants.get(lhs), constants.get(rhs)) {
            (None, Some(key)) => (*lhs, *rhs, key.value),
            (Some(key), None) => (*rhs, *lhs, key.value),
            _ => return None,
        },
        _ => unreachable!(),
    };
    let selector_width = register_types.get(&selector)?.width();
    if selector_width == 0
        || selector_width > 64
        || register_types.get(&key_reg)?.width() != selector_width
        || key & !mask_for_width(selector_width) != 0
    {
        return None;
    }
    covered_indices.insert(compare_idx);
    if let Some(&key_idx) = defs.get(&key_reg) {
        covered_indices.insert(key_idx);
    }
    Some(DenseLookupCondition {
        selector,
        selector_width,
        key,
        covered_indices,
    })
}

fn find_priority_encode_plans(
    block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    uses: &HashMap<RegisterId, Vec<SirUseSite>>,
) -> PriorityEncodePlans {
    const MIN_PRIORITY_ENCODE_WIDTH: usize = 32;

    let mut defs: HashMap<RegisterId, usize> = HashMap::default();
    let mut else_children = HashSet::default();
    for (idx, inst) in block.instructions.iter().enumerate() {
        if let Some(dst) = sir_def_reg(inst) {
            defs.insert(dst, idx);
        }
    }
    for inst in &block.instructions {
        if let SIRInstruction::Mux(_, _, _, else_val) = inst
            && defs
                .get(else_val)
                .is_some_and(|&idx| matches!(block.instructions[idx], SIRInstruction::Mux(..)))
        {
            else_children.insert(*else_val);
        }
    }

    let mut plans = PriorityEncodePlans::default();
    for (root_idx, inst) in block.instructions.iter().enumerate().rev() {
        let SIRInstruction::Mux(root_dst, ..) = inst else {
            continue;
        };
        if else_children.contains(root_dst) || plans.skip_indices.contains(&root_idx) {
            continue;
        }
        let Some((plan, required_indices, optional_indices)) =
            collect_priority_encode_candidate(block, &defs, root_idx, *root_dst)
        else {
            continue;
        };
        if plan.width < MIN_PRIORITY_ENCODE_WIDTH
            || required_indices
                .iter()
                .any(|idx| plans.skip_indices.contains(idx))
        {
            continue;
        }
        if !required_indices.iter().all(|idx| {
            *idx == root_idx || def_used_only_by_candidate(block, *idx, &required_indices, uses)
        }) {
            continue;
        }

        for idx in required_indices {
            plans.skip_indices.insert(idx);
        }
        for idx in optional_indices {
            if def_used_only_by_candidate(block, idx, &plans.skip_indices, uses) {
                plans.skip_indices.insert(idx);
            }
        }
        plans.roots.insert(plan.root_idx, plan);
    }
    plans
}

fn collect_priority_encode_candidate(
    block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    defs: &HashMap<RegisterId, usize>,
    root_idx: usize,
    root_dst: RegisterId,
) -> Option<(PriorityEncodePlan, HashSet<usize>, HashSet<usize>)> {
    let mut cursor = root_dst;
    let mut default_reg = None;
    let mut default_value = None;
    let mut source = None;
    let mut items: Vec<(usize, usize)> = Vec::new();
    let mut required_indices = HashSet::default();
    let mut optional_indices = HashSet::default();

    loop {
        let &mux_idx = defs.get(&cursor)?;
        let SIRInstruction::Mux(dst, cond, then_val, else_val) = &block.instructions[mux_idx]
        else {
            return None;
        };
        if *dst != cursor {
            return None;
        }

        let (cond_idx, acc_eq_idx, guard, matched_default_reg, matched_default_value) =
            match_priority_encode_cond(block, defs, *cond, *else_val)?;
        if let Some(reg) = default_reg {
            if reg != matched_default_reg {
                return None;
            }
        } else {
            default_reg = Some(matched_default_reg);
            default_value = Some(matched_default_value);
        }

        let (guard_src, bit_index, guard_required, guard_optional) =
            match_priority_bit_guard(block, defs, guard)?;
        if let Some(src) = source {
            if src != guard_src {
                return None;
            }
        } else {
            source = Some(guard_src);
        }

        let then_value = sir_imm_u64(block, defs, *then_val)? as usize;
        if let Some(&then_idx) = defs.get(then_val) {
            optional_indices.insert(then_idx);
        }
        required_indices.insert(mux_idx);
        required_indices.insert(cond_idx);
        required_indices.insert(acc_eq_idx);
        required_indices.extend(guard_required);
        optional_indices.extend(guard_optional);
        items.push((then_value, bit_index));

        if let Some(&prev_idx) = defs.get(else_val)
            && matches!(block.instructions[prev_idx], SIRInstruction::Mux(..))
        {
            cursor = *else_val;
            continue;
        }
        if Some(*else_val) != default_reg {
            return None;
        }
        if let Some(&default_idx) = defs.get(else_val) {
            optional_indices.insert(default_idx);
        }
        break;
    }

    let width = default_value? as usize;
    if width != items.len() {
        return None;
    }
    for (stage, (then_value, bit_index)) in items.into_iter().enumerate() {
        if then_value != width - 1 - stage || bit_index != stage {
            return None;
        }
    }

    Some((
        PriorityEncodePlan {
            root_idx,
            dst: root_dst,
            src: source?,
            width,
        },
        required_indices,
        optional_indices,
    ))
}

fn match_priority_encode_cond(
    block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    defs: &HashMap<RegisterId, usize>,
    cond: RegisterId,
    prev_acc: RegisterId,
) -> Option<(usize, usize, RegisterId, RegisterId, u64)> {
    let &cond_idx = defs.get(&cond)?;
    let SIRInstruction::Binary(_, lhs, BinaryOp::LogicAnd, rhs) = block.instructions[cond_idx]
    else {
        return None;
    };
    if let Some((eq_idx, default_reg, default_value)) =
        match_acc_eq_default(block, defs, lhs, prev_acc)
    {
        return Some((cond_idx, eq_idx, rhs, default_reg, default_value));
    }
    if let Some((eq_idx, default_reg, default_value)) =
        match_acc_eq_default(block, defs, rhs, prev_acc)
    {
        return Some((cond_idx, eq_idx, lhs, default_reg, default_value));
    }
    None
}

fn match_acc_eq_default(
    block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    defs: &HashMap<RegisterId, usize>,
    eq_reg: RegisterId,
    prev_acc: RegisterId,
) -> Option<(usize, RegisterId, u64)> {
    let &eq_idx = defs.get(&eq_reg)?;
    let SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs) = block.instructions[eq_idx] else {
        return None;
    };
    if lhs == prev_acc {
        let value = sir_imm_u64(block, defs, rhs)?;
        return Some((eq_idx, rhs, value));
    }
    if rhs == prev_acc {
        let value = sir_imm_u64(block, defs, lhs)?;
        return Some((eq_idx, lhs, value));
    }
    None
}

fn match_priority_bit_guard(
    block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    defs: &HashMap<RegisterId, usize>,
    guard: RegisterId,
) -> Option<(RegisterId, usize, Vec<usize>, Vec<usize>)> {
    let &eq_idx = defs.get(&guard)?;
    let SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs) = block.instructions[eq_idx] else {
        return None;
    };

    let bit_reg = if sir_imm_u64(block, defs, lhs) == Some(1) {
        if let Some(&idx) = defs.get(&lhs) {
            let (_, _, mut required, mut optional) = match_bit_extract(block, defs, rhs)?;
            optional.push(idx);
            required.push(eq_idx);
            let (src, bit_index, _, _) = match_bit_extract(block, defs, rhs)?;
            return Some((src, bit_index, required, optional));
        }
        rhs
    } else if sir_imm_u64(block, defs, rhs) == Some(1) {
        if let Some(&idx) = defs.get(&rhs) {
            let (_, _, mut required, mut optional) = match_bit_extract(block, defs, lhs)?;
            optional.push(idx);
            required.push(eq_idx);
            let (src, bit_index, _, _) = match_bit_extract(block, defs, lhs)?;
            return Some((src, bit_index, required, optional));
        }
        lhs
    } else {
        return None;
    };
    let (src, bit_index, mut required, optional) = match_bit_extract(block, defs, bit_reg)?;
    required.push(eq_idx);
    Some((src, bit_index, required, optional))
}

fn match_bit_extract(
    block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    defs: &HashMap<RegisterId, usize>,
    bit_reg: RegisterId,
) -> Option<(RegisterId, usize, Vec<usize>, Vec<usize>)> {
    let mut required = Vec::new();
    let mut optional = Vec::new();
    let &and_idx = defs.get(&bit_reg)?;
    let SIRInstruction::Binary(_, and_lhs, BinaryOp::And, and_rhs) = block.instructions[and_idx]
    else {
        return None;
    };
    required.push(and_idx);
    let shifted = if sir_imm_u64(block, defs, and_lhs) == Some(1) {
        if let Some(&idx) = defs.get(&and_lhs) {
            optional.push(idx);
        }
        and_rhs
    } else if sir_imm_u64(block, defs, and_rhs) == Some(1) {
        if let Some(&idx) = defs.get(&and_rhs) {
            optional.push(idx);
        }
        and_lhs
    } else {
        return None;
    };

    let Some(&shr_idx) = defs.get(&shifted) else {
        return Some((shifted, 0, required, optional));
    };
    if let SIRInstruction::Binary(_, src, BinaryOp::Shr, shift_reg) = block.instructions[shr_idx] {
        let bit_index = sir_imm_u64(block, defs, shift_reg)? as usize;
        required.push(shr_idx);
        if let Some(&idx) = defs.get(&shift_reg) {
            optional.push(idx);
        }
        Some((src, bit_index, required, optional))
    } else {
        Some((shifted, 0, required, optional))
    }
}

fn sir_imm_u64(
    block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    defs: &HashMap<RegisterId, usize>,
    reg: RegisterId,
) -> Option<u64> {
    let &idx = defs.get(&reg)?;
    let SIRInstruction::Imm(_, value) = &block.instructions[idx] else {
        return None;
    };
    if value.mask != num_bigint::BigUint::ZERO {
        return None;
    }
    let digits = value.payload.to_u64_digits();
    match digits.as_slice() {
        [] => Some(0),
        [value] => Some(*value),
        _ => None,
    }
}

fn def_used_only_by_candidate(
    block: &crate::BasicBlock<RegionedAbsoluteAddr>,
    idx: usize,
    candidate_indices: &HashSet<usize>,
    uses: &HashMap<RegisterId, Vec<SirUseSite>>,
) -> bool {
    let Some(def) = sir_def_reg(&block.instructions[idx]) else {
        return true;
    };
    uses.get(&def).is_none_or(|sites| {
        sites.iter().all(|site| {
            site.block == block.id
                && site
                    .inst_idx
                    .is_some_and(|use_idx| candidate_indices.contains(&use_idx))
        })
    })
}

fn emit_dense_lookup(
    ctx: &mut ISelContext<'_>,
    block: &mut MBlock,
    plan: &DenseLookupPlan,
    cache: &mut DenseLookupEmitCache,
) {
    debug_assert_eq!(
        ctx.sir_width(&plan.default),
        ctx.sir_width(&plan.dst),
        "full-domain lookup default must have the result width",
    );
    let table = plan
        .table
        .expect("dense lookup table must be interned before instruction selection");
    let byte_index = *cache
        .byte_indices
        .entry((plan.selector, plan.selector_width))
        .or_insert_with(|| {
            let selector = ctx.reg_map.get(plan.selector);
            let masked = ctx.alloc_vreg(SpillDesc::transient());
            // The SIR type width is not enough to make a memory access safe:
            // materialized registers can still carry stale upper bits.  Keep
            // this explicit even when known-bits analysis could elide it.
            block.push(MInst::AndImm {
                dst: masked,
                src: selector,
                imm: mask_for_width(plan.selector_width),
            });
            ctx.known_bits.insert(masked, plan.selector_width);
            let scaled = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShlImm {
                dst: scaled,
                src: masked,
                imm: 3,
            });
            scaled
        });
    let table_addr = *cache.table_addrs.entry(table).or_insert_with(|| {
        let table_addr = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::LoadConstantTableAddr {
            dst: table_addr,
            table,
        });
        table_addr
    });
    let dst = ctx.reg_map.get(plan.dst);
    block.push(MInst::LoadPtrIndexed {
        dst,
        ptr: table_addr,
        offset: 0,
        index: byte_index,
        size: OpSize::S64,
    });
    ctx.known_bits.insert(dst, ctx.sir_width(&plan.dst));
}

fn emit_priority_encode(ctx: &mut ISelContext<'_>, block: &mut MBlock, plan: &PriorityEncodePlan) {
    let dst = ctx.reg_map.get(plan.dst);
    let n_chunks = plan.width.div_ceil(64).max(1);
    let chunks = if ctx.wide_regs.contains_key(&plan.src) {
        ctx.get_wide_chunks(&plan.src, block)
    } else {
        vec![(ctx.reg_map.get(plan.src), ctx.sir_width(&plan.src).min(64))]
    };

    let mut result = ctx.alloc_vreg(SpillDesc::remat(plan.width as u64));
    block.push(MInst::LoadImm {
        dst: result,
        value: plan.width as u64,
    });

    for chunk_idx in 0..n_chunks {
        let chunk_bits = if chunk_idx + 1 == n_chunks {
            plan.width - chunk_idx * 64
        } else {
            64
        };
        let raw_chunk = chunks.get(chunk_idx).map(|(v, _)| *v).unwrap_or_else(|| {
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            zero
        });
        let chunk = if chunk_bits < 64 {
            let masked = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked, raw_chunk, mask_for_width(chunk_bits));
            masked
        } else {
            raw_chunk
        };

        let nonzero = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::CmpImm {
            dst: nonzero,
            lhs: chunk,
            imm: 0,
            kind: CmpKind::Ne,
        });
        let bsr = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Bsr {
            dst: bsr,
            src: chunk,
        });
        let high_index = (plan.width - 1 - chunk_idx * 64) as u64;
        let high = ctx.alloc_vreg(SpillDesc::remat(high_index));
        block.push(MInst::LoadImm {
            dst: high,
            value: high_index,
        });
        let candidate = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Sub {
            dst: candidate,
            lhs: high,
            rhs: bsr,
        });
        let next = if chunk_idx + 1 == n_chunks {
            dst
        } else {
            ctx.alloc_vreg(SpillDesc::transient())
        };
        block.push(MInst::Select {
            dst: next,
            cond: nonzero,
            true_val: candidate,
            false_val: result,
        });
        result = next;
    }

    let known_bits = if plan.width == 0 {
        0
    } else {
        (usize::BITS as usize - plan.width.leading_zeros() as usize).min(ctx.sir_width(&plan.dst))
    };
    ctx.known_bits.insert(dst, known_bits);
}

fn sign_extend_scalar(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    source: VReg,
    width: usize,
) -> VReg {
    if width >= 64 {
        return source;
    }
    debug_assert!(width > 0);
    let shift = (64 - width) as u8;
    let shifted_up = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShlImm {
        dst: shifted_up,
        src: source,
        imm: shift,
    });
    let sign_extended = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::SarImm {
        dst: sign_extended,
        src: shifted_up,
        imm: shift,
    });
    sign_extended
}

/// Sign-extend a pair of operands for signed comparison.
/// For widths < 64, shifts left then arithmetic-shifts right to propagate the sign bit.
fn sign_extend_pair(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    lhs_sir: &RegisterId,
    rhs_sir: &RegisterId,
    lhs_vreg: VReg,
    rhs_vreg: VReg,
) -> (VReg, VReg) {
    let lw = ctx.sir_width(lhs_sir);
    let rw = ctx.sir_width(rhs_sir);
    let width = lw.max(rw);

    if width >= 64 {
        return (lhs_vreg, rhs_vreg);
    }

    let shift = (64 - width) as u8;

    let sign_extend_with_imm = |ctx: &mut ISelContext, block: &mut MBlock, src: VReg| -> VReg {
        let shifted_up = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::ShlImm {
            dst: shifted_up,
            src,
            imm: shift,
        });
        let sign_extended = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::SarImm {
            dst: sign_extended,
            src: shifted_up,
            imm: shift,
        });
        sign_extended
    };

    let sl = sign_extend_with_imm(ctx, block, lhs_vreg);
    let sr = sign_extend_with_imm(ctx, block, rhs_vreg);
    (sl, sr)
}

fn lower_selector_branch_table(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    plan: &SelectorBranchTablePlan,
) {
    if plan
        .targets
        .first()
        .is_some_and(|target| plan.targets.iter().all(|candidate| candidate == target))
    {
        block.push(MInst::Jump {
            target: BlockId(plan.targets[0].0 as u32),
        });
        return;
    }
    let selector = ctx.reg_map.get(plan.selector);
    let normalized = ctx.alloc_vreg(SpillDesc::transient());
    ctx.emit_and_imm(
        block,
        normalized,
        selector,
        mask_for_width(plan.selector_width),
    );
    block.push(MInst::JumpTable {
        index: normalized,
        targets: plan
            .targets
            .iter()
            .map(|target| BlockId(target.0 as u32))
            .collect(),
    });
}

fn lower_terminator(ctx: &mut ISelContext, block: &mut MBlock, term: &SIRTerminator) {
    match term {
        SIRTerminator::Jump(target, _args) => {
            // Block args are handled via phi nodes (built in a second pass).
            block.push(MInst::Jump {
                target: BlockId(target.0 as u32),
            });
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            let cond_vreg = lower_branch_condition(ctx, block, *cond);
            if ctx.trace_regs.contains(cond) {
                tracing::debug!(
                    "[isel-trace] terminator branch cond r{} -> {}",
                    cond.0,
                    cond_vreg
                );
            }
            block.push(MInst::Branch {
                cond: cond_vreg,
                true_bb: BlockId(true_block.0.0 as u32),
                false_bb: BlockId(false_block.0.0 as u32),
            });
        }
        SIRTerminator::Switch {
            selector,
            cases,
            default,
        } => {
            let selector_width = ctx.sir_width(selector);
            debug_assert!((1..=8).contains(&selector_width));
            let mut targets = vec![BlockId(default.0 as u32); 1usize << selector_width];
            for case in cases {
                let digits = case.value.to_u64_digits();
                let index = match digits.as_slice() {
                    [] => 0,
                    [value] => *value as usize,
                    _ => unreachable!("verified switch key fits eight bits"),
                };
                targets[index] = BlockId(case.target.0 as u32);
            }
            if targets
                .first()
                .is_some_and(|target| targets.iter().all(|candidate| candidate == target))
            {
                block.push(MInst::Jump { target: targets[0] });
                return;
            }
            let selector = ctx.reg_map.get(*selector);
            let normalized = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, normalized, selector, mask_for_width(selector_width));
            block.push(MInst::JumpTable {
                index: normalized,
                targets: targets.into(),
            });
        }
        SIRTerminator::Return => {
            block.push(MInst::Return);
        }
        SIRTerminator::Error(code) => {
            block.push(MInst::ReturnError { code: *code });
        }
    }
}

fn lower_branch_condition(ctx: &mut ISelContext, block: &mut MBlock, cond: RegisterId) -> VReg {
    let Some(chunks) = ctx.wide_regs.get(&cond).cloned() else {
        return ctx.reg_map.get(cond);
    };

    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });

    let mut any_set: Option<VReg> = None;
    for (chunk, width) in chunks {
        let value = if width < 64 {
            let masked = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked, chunk, mask_for_width(width));
            masked
        } else {
            chunk
        };
        let nonzero = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: nonzero,
            lhs: value,
            rhs: zero,
            kind: CmpKind::Ne,
        });
        ctx.known_bits.insert(nonzero, 1);

        any_set = Some(match any_set {
            Some(prev) => {
                let merged = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: merged,
                    lhs: prev,
                    rhs: nonzero,
                });
                ctx.known_bits.insert(merged, 1);
                merged
            }
            None => nonzero,
        });
    }

    any_set.unwrap_or(zero)
}

#[cfg(test)]
mod tests;
