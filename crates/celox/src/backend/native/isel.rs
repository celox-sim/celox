//! Instruction Selection: lowers SIR (bit-level SSA) to MIR (word-level SSA).
//!
//! Supports 2-state and 4-state (IEEE 1800) with full mask propagation.
//! Handles arbitrary widths: narrow (≤64-bit) and wide (>64-bit, chunk-based).

use crate::ir::{
    BinaryOp, ExecutionUnit, RegisterId, RegisterType, SIRInstruction, SIROffset, SIRTerminator,
    UnaryOp,
};
use crate::ir::{RegionedAbsoluteAddr, STABLE_REGION};

use super::mir::*;
use crate::backend::MemoryLayout;
use crate::{HashMap, HashSet};

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

fn parse_trace_sir_regs() -> HashSet<RegisterId> {
    std::env::var_os("CELOX_ISEL_TRACE_REGS")
        .or_else(|| std::env::var_os("CELOX_ISEL_TRACE_REG"))
        .map(|raw| {
            raw.to_string_lossy()
                .split(',')
                .filter_map(|part| part.trim().parse::<usize>().ok())
                .map(RegisterId)
                .collect()
        })
        .unwrap_or_default()
}

/// Lower a single SIR execution unit to a MIR function.
///
/// Only handles 2-state values ≤64 bits for now.
pub fn lower_execution_unit(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    layout: &MemoryLayout,
    four_state: bool,
) -> MFunction {
    if cfg!(debug_assertions) || std::env::var_os("CELOX_SIR_VERIFY").is_some() {
        if let Err(error) = eu.verify_result() {
            panic!("before native ISel: {error}");
        }
    }
    let mut vregs = VRegAllocator::new();
    let mut spill_descs: Vec<SpillDesc> = Vec::new();
    let max_sir_regs = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0) + 1;
    let mut reg_map = RegMap::new(max_sir_regs);
    let trace_regs = parse_trace_sir_regs();

    // Pre-allocate a VReg for each SIR register
    for sir_reg_id in eu.register_map.keys() {
        let vreg = vregs.alloc();
        reg_map.set(*sir_reg_id, vreg);
        if trace_regs.contains(sir_reg_id) {
            eprintln!("[isel-trace] prealloc r{} -> {}", sir_reg_id.0, vreg);
        }
        // Spill desc will be filled during instruction lowering.
        // For now, default to transient.
        spill_descs.push(SpillDesc::transient());
    }

    let mut func = MFunction::new(vregs.clone(), spill_descs);
    let block_ids = ordered_sir_blocks(eu);
    let native_priority_encode = !four_state;
    let sir_use_sites = if native_priority_encode {
        Some(collect_sir_use_sites(eu))
    } else {
        None
    };
    let mut dense_lookup_plans_by_block: HashMap<crate::ir::BlockId, DenseLookupPlans> =
        HashMap::default();
    if !four_state {
        let constants = collect_exact_sir_constants(eu);
        let uses = sir_use_sites
            .as_ref()
            .expect("two-state lookup planning must collect SIR uses");
        for &block_id in &block_ids {
            let block = &eu.blocks[&block_id];
            let mut plans = find_dense_lookup_plans(block, &eu.register_map, &constants, uses);
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
    let mut sir_exit_mir_blocks: std::collections::HashMap<crate::ir::BlockId, BlockId> =
        std::collections::HashMap::new();

    let mut mask_map = RegMap::new(max_sir_regs);
    // Pre-allocate mask VRegs for 4-state
    if four_state {
        for sir_reg_id in eu.register_map.keys() {
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
        wide_masks: WideRegMap::default(),
        trigger_only_seen: HashSet::default(),
        trace_regs,
    };
    // Pre-seed wide block params so instructions in those blocks can read the
    // full chunked value before phi nodes are materialized in a later pass.
    for sir_block in eu.blocks.values() {
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
                ctx.wide_regs.insert(param_reg, chunks);
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
    let mut mask_phi_sources: std::collections::HashMap<
        BlockId,
        Vec<(BlockId, usize, usize, VReg)>,
    > = std::collections::HashMap::new();

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
        let mut lookup_emit_cache = DenseLookupEmitCache::default();
        let sir_defs = collect_sir_defs(sir_block);

        // Lower instructions
        for (inst_idx, inst) in sir_block.instructions.iter().enumerate() {
            if let Some(dst) = sir_def_reg(inst)
                && ctx.trace_regs.contains(&dst)
            {
                eprintln!(
                    "[isel-trace] b{} inst {} lowering r{}: {}",
                    sir_block.id.0, inst_idx, dst.0, inst
                );
            }
            if lookup_plans.skip_indices.contains(&inst_idx) {
                if let Some(plan) = lookup_plans.roots.get(&inst_idx) {
                    if ctx.trace_regs.contains(&plan.dst) {
                        eprintln!(
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
                        eprintln!(
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
                    eprintln!(
                        "[isel-trace] b{} inst {} skipped r{} without root",
                        sir_block.id.0, inst_idx, dst.0
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
                        offset:
                            crate::backend::memory_layout::STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET
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
            } else {
                lower_instruction(&mut ctx, &mut mblock, inst, sir_block, &sir_defs);
            }

            // Track known bit width for redundant mask elimination.
            let dst_reg = match inst {
                SIRInstruction::Imm(d, _)
                | SIRInstruction::Binary(d, _, _, _)
                | SIRInstruction::Unary(d, _, _)
                | SIRInstruction::Load(d, _, _, _)
                | SIRInstruction::Concat(d, _)
                | SIRInstruction::Slice(d, _, _, _)
                | SIRInstruction::Mux(d, _, _, _) => Some(*d),
                SIRInstruction::Store(..)
                | SIRInstruction::Commit(..)
                | SIRInstruction::RuntimeEvent { .. }
                | SIRInstruction::CombCaptureEvent { .. }
                | SIRInstruction::CombCaptureEnableIfChanged { .. } => None,
            };
            if let Some(dr) = dst_reg {
                let w = ctx.sir_width(&dr);
                if w <= 64 {
                    let vreg = ctx.reg_map.get(dr);
                    ctx.known_bits.insert(vreg, w);
                    if ctx.trace_regs.contains(&dr) {
                        eprintln!(
                            "[isel-trace] b{} inst {} after r{} -> {} known_bits={}",
                            sir_block.id.0, inst_idx, dr.0, vreg, w
                        );
                    }
                }
            }
        }

        // Lower terminator
        lower_terminator(&mut ctx, &mut mblock, &sir_block.terminator);
        let pred_mir_id = mblock.id;
        sir_exit_mir_blocks.insert(sir_block_id, pred_mir_id);

        // Capture mask phi sources from this block's terminator (before mask_map changes)
        if four_state {
            let edges: Vec<(crate::ir::BlockId, &[RegisterId])> = match &sir_block.terminator {
                SIRTerminator::Jump(target, args) => vec![(*target, args.as_slice())],
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => vec![
                    (true_block.0, true_block.1.as_slice()),
                    (false_block.0, false_block.1.as_slice()),
                ],
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
        use std::collections::HashMap;
        // Collect phi sources: target_block → [(pred_block, param_idx, chunk_idx, arg_vreg)]
        let mut phi_sources: HashMap<BlockId, Vec<(BlockId, usize, usize, VReg)>> = HashMap::new();
        for &sir_block_id in &block_ids {
            let sir_block = &eu.blocks[&sir_block_id];
            let pred_mir_id = sir_exit_mir_blocks
                .get(&sir_block_id)
                .copied()
                .unwrap_or(BlockId(sir_block_id.0 as u32));
            let edges: Vec<(crate::ir::BlockId, &[RegisterId])> = match &sir_block.terminator {
                SIRTerminator::Jump(target, args) => vec![(*target, args.as_slice())],
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => vec![
                    (true_block.0, true_block.1.as_slice()),
                    (false_block.0, false_block.1.as_slice()),
                ],
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
                let sir_block_id = crate::ir::BlockId(mblock.id.0 as usize);
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

fn ordered_sir_blocks(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> Vec<crate::ir::BlockId> {
    fn successors(term: &SIRTerminator) -> Vec<crate::ir::BlockId> {
        match term {
            SIRTerminator::Jump(target, _) => vec![*target],
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => vec![true_block.0, false_block.0],
            SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
        }
    }

    fn visit_from(
        eu: &ExecutionUnit<RegionedAbsoluteAddr>,
        start: crate::ir::BlockId,
        visited: &mut HashSet<crate::ir::BlockId>,
        postorder: &mut Vec<crate::ir::BlockId>,
    ) {
        let mut stack = vec![(start, false)];
        while let Some((block_id, expanded)) = stack.pop() {
            if !eu.blocks.contains_key(&block_id) {
                continue;
            }
            if expanded {
                postorder.push(block_id);
                continue;
            }
            if !visited.insert(block_id) {
                continue;
            }
            stack.push((block_id, true));
            let mut succs = successors(&eu.blocks[&block_id].terminator);
            succs.reverse();
            for succ in succs {
                if !visited.contains(&succ) {
                    stack.push((succ, false));
                }
            }
        }
    }

    let mut visited = HashSet::default();
    let mut postorder = Vec::new();
    visit_from(eu, eu.entry_block_id, &mut visited, &mut postorder);

    let mut sorted_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    sorted_ids.sort();
    for block_id in sorted_ids {
        if !visited.contains(&block_id) {
            visit_from(eu, block_id, &mut visited, &mut postorder);
        }
    }

    postorder.reverse();
    postorder
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

/// Tracks wide (>64-bit) register values as multiple 64-bit chunks.
/// Key: SIR RegisterId, Value: chunks in LSB-first order (vreg, width_bits).
/// Used for Concat results and multi-word arithmetic (Shl/And/Or/BitNot on >64-bit values).
type WideRegMap = crate::HashMap<RegisterId, Vec<(VReg, usize)>>;

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
    wide_masks: WideRegMap,
    /// Width=0 trigger-only stores do not write memory. Within one MIR block,
    /// rechecking the same physical byte for the same trigger id is redundant
    /// until a real Store/Commit may change memory.
    trigger_only_seen: HashSet<(i32, usize)>,
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

    /// Resolve the mask byte offset for a variable.
    /// The mask is stored immediately after the value in memory.
    fn mask_byte_offset(&self, addr: &RegionedAbsoluteAddr, bit_offset: usize) -> i32 {
        let abs_addr = addr.absolute_addr();
        let width = self.layout.widths.get(&abs_addr).copied().unwrap_or(0);
        let byte_size = crate::backend::get_byte_size(width);
        self.byte_offset(addr, bit_offset) + byte_size as i32
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
        let abs_addr = addr.absolute_addr();
        let base = match addr.region {
            STABLE_REGION => *self.layout.offsets.get(&abs_addr).unwrap_or(&0),
            crate::ir::SPARSE_WORKING_REGION => {
                self.layout.sparse_base_offset
                    + *self.layout.sparse_offsets.get(&abs_addr).unwrap_or(&0)
            }
            _ => {
                self.layout.working_base_offset
                    + *self.layout.working_offsets.get(&abs_addr).unwrap_or(&0)
            }
        };
        (base + bit_offset / 8) as i32
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

        if (signed >= i32::MIN as i64 && signed <= i32::MAX as i64) || imm <= u32::MAX as u64 {
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
        block.push(MInst::Mov { dst, src });
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
        block.push(MInst::Mov { dst, src });
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

    /// Store wide chunks for a SIR register in the wide_regs map.
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
        self.wide_regs.insert(reg, chunks);
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

fn load_runtime_event_ptr(ctx: &mut ISelContext, block: &mut MBlock) -> VReg {
    let event_ptr = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Load {
        dst: event_ptr,
        base: BaseReg::SimState,
        offset: crate::backend::memory_layout::STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET as i32,
        size: OpSize::S64,
    });
    event_ptr
}

fn load_runtime_event_ptr_and_comb_capture_enabled(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    site_id: u32,
) -> (VReg, VReg) {
    let event_ptr = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Load {
        dst: event_ptr,
        base: BaseReg::SimState,
        offset: crate::backend::memory_layout::STATE_HEADER_RUNTIME_EVENT_ADDR_OFFSET as i32,
        size: OpSize::S64,
    });
    let enabled_ptr = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Load {
        dst: enabled_ptr,
        base: BaseReg::SimState,
        offset: crate::backend::memory_layout::STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET as i32,
        size: OpSize::S64,
    });
    let enabled_byte = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::LoadPtr {
        dst: enabled_byte,
        ptr: enabled_ptr,
        offset: site_id as i32,
        size: OpSize::S8,
    });
    let enabled = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::CmpImm {
        dst: enabled,
        lhs: enabled_byte,
        imm: 0,
        kind: CmpKind::Ne,
    });
    (event_ptr, enabled)
}

fn emit_enable_comb_capture_sites(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    changed: VReg,
    site_ids: &[u32],
) {
    let enabled_ptr = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Load {
        dst: enabled_ptr,
        base: BaseReg::SimState,
        offset: crate::backend::memory_layout::STATE_HEADER_COMB_CAPTURE_ENABLED_ADDR_OFFSET as i32,
        size: OpSize::S64,
    });
    let one = ctx.alloc_vreg(SpillDesc::remat(1));
    block.push(MInst::LoadImm { dst: one, value: 1 });
    for &site_id in site_ids {
        let old = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::LoadPtr {
            dst: old,
            ptr: enabled_ptr,
            offset: site_id as i32,
            size: OpSize::S8,
        });
        let next = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Select {
            dst: next,
            cond: changed,
            true_val: one,
            false_val: old,
        });
        block.push(MInst::StorePtr {
            ptr: enabled_ptr,
            offset: site_id as i32,
            src: next,
            size: OpSize::S8,
        });
    }
}

fn emit_enable_comb_capture_sites_if_regs_changed(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    old: RegisterId,
    new: RegisterId,
    site_ids: &[u32],
) {
    if site_ids.is_empty() {
        return;
    }

    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let mut changed = zero;

    if ctx.sir_width(&old) > 64 || ctx.sir_width(&new) > 64 {
        let old_chunks = ctx.get_wide_chunks(&old, block);
        let new_chunks = ctx.get_wide_chunks(&new, block);
        let chunk_count = old_chunks.len().max(new_chunks.len());
        let compare_width = ctx.sir_width(&old).max(ctx.sir_width(&new));
        for idx in 0..chunk_count {
            let old_chunk = ctx.wide_chunk_or_zero(&old_chunks, idx, block);
            let new_chunk = ctx.wide_chunk_or_zero(&new_chunks, idx, block);
            let chunk_width = compare_width.saturating_sub(idx * 64).min(64);
            let (old_cmp, new_cmp) = if chunk_width < 64 {
                let masked_old = ctx.alloc_vreg(SpillDesc::transient());
                let masked_new = ctx.alloc_vreg(SpillDesc::transient());
                let mask = mask_for_width(chunk_width);
                ctx.emit_and_imm(block, masked_old, old_chunk, mask);
                ctx.emit_and_imm(block, masked_new, new_chunk, mask);
                (masked_old, masked_new)
            } else {
                (old_chunk, new_chunk)
            };
            let chunk_changed = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: chunk_changed,
                lhs: old_cmp,
                rhs: new_cmp,
                kind: CmpKind::Ne,
            });
            let next = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: next,
                lhs: changed,
                rhs: chunk_changed,
            });
            changed = next;
        }

        if ctx.four_state {
            let old_masks = get_wide_mask_chunks(ctx, block, &old, chunk_count);
            let new_masks = get_wide_mask_chunks(ctx, block, &new, chunk_count);
            for (idx, (old_mask, new_mask)) in old_masks.into_iter().zip(new_masks).enumerate() {
                let chunk_width = compare_width.saturating_sub(idx * 64).min(64);
                let (old_cmp, new_cmp) = if chunk_width < 64 {
                    let masked_old = ctx.alloc_vreg(SpillDesc::transient());
                    let masked_new = ctx.alloc_vreg(SpillDesc::transient());
                    let mask = mask_for_width(chunk_width);
                    ctx.emit_and_imm(block, masked_old, old_mask, mask);
                    ctx.emit_and_imm(block, masked_new, new_mask, mask);
                    (masked_old, masked_new)
                } else {
                    (old_mask, new_mask)
                };
                let mask_changed = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: mask_changed,
                    lhs: old_cmp,
                    rhs: new_cmp,
                    kind: CmpKind::Ne,
                });
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: next,
                    lhs: changed,
                    rhs: mask_changed,
                });
                changed = next;
            }
        }
    } else {
        let value_changed = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: value_changed,
            lhs: ctx.reg_map.get(old),
            rhs: ctx.reg_map.get(new),
            kind: CmpKind::Ne,
        });
        changed = value_changed;

        if ctx.four_state {
            let old_mask = ctx.get_mask(old, block);
            let new_mask = ctx.get_mask(new, block);
            let mask_changed = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: mask_changed,
                lhs: old_mask,
                rhs: new_mask,
                kind: CmpKind::Ne,
            });
            let next = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: next,
                lhs: changed,
                rhs: mask_changed,
            });
            changed = next;
        }
    }

    emit_enable_comb_capture_sites(ctx, block, changed, site_ids);
}

fn collect_static_comb_store_byte_probes(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    bit_offset: usize,
    width_bits: usize,
    mask_region: bool,
) -> Vec<(VReg, i32, OpSize)> {
    let start_byte = bit_offset / 8;
    let byte_len = ((bit_offset % 8) + width_bits).div_ceil(8);
    let mut probes = Vec::new();
    let mut byte_pos = 0usize;

    while byte_pos < byte_len {
        let remaining = byte_len - byte_pos;
        let size = if remaining >= 8 {
            OpSize::S64
        } else if remaining >= 4 {
            OpSize::S32
        } else if remaining >= 2 {
            OpSize::S16
        } else {
            OpSize::S8
        };
        let offset_bits = (start_byte + byte_pos) * 8;
        let byte_off = if mask_region {
            ctx.mask_byte_offset(addr, offset_bits)
        } else {
            ctx.byte_offset(addr, offset_bits)
        };
        let old = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Load {
            dst: old,
            base: BaseReg::SimState,
            offset: byte_off,
            size,
        });
        probes.push((old, byte_off, size));
        byte_pos += size.bytes() as usize;
    }

    probes
}

fn emit_enable_comb_capture_sites_if_byte_probes_changed(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    probes: Vec<(VReg, i32, OpSize)>,
    site_ids: &[u32],
) {
    if probes.is_empty() {
        return;
    }

    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let mut changed = zero;
    for (old, byte_off, size) in probes {
        let new = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Load {
            dst: new,
            base: BaseReg::SimState,
            offset: byte_off,
            size,
        });
        let chunk_changed = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: chunk_changed,
            lhs: old,
            rhs: new,
            kind: CmpKind::Ne,
        });
        let next_changed = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: next_changed,
            lhs: changed,
            rhs: chunk_changed,
        });
        changed = next_changed;
    }
    emit_enable_comb_capture_sites(ctx, block, changed, site_ids);
}

fn lower_runtime_event_write(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    event_ptr: VReg,
    site_id: u32,
    args: &[RegisterId],
) {
    use crate::backend::memory_layout::{
        RUNTIME_EVENT_HEADER_SIZE, RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET,
        RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET, RUNTIME_EVENT_SLOT_SEQ_OFFSET,
        RUNTIME_EVENT_SLOT_SITE_OFFSET, RUNTIME_EVENT_WRITING,
    };

    let seq_v = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::LoadPtr {
        dst: seq_v,
        ptr: event_ptr,
        offset: 0,
        size: OpSize::S64,
    });
    let mask_v = ctx.alloc_vreg(SpillDesc::remat(
        (ctx.layout.runtime_event_capacity as u64) - 1,
    ));
    block.push(MInst::LoadImm {
        dst: mask_v,
        value: (ctx.layout.runtime_event_capacity as u64) - 1,
    });
    let slot_idx = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::And {
        dst: slot_idx,
        lhs: seq_v,
        rhs: mask_v,
    });
    let slot_size_v = ctx.alloc_vreg(SpillDesc::remat(ctx.layout.runtime_event_slot_size as u64));
    block.push(MInst::LoadImm {
        dst: slot_size_v,
        value: ctx.layout.runtime_event_slot_size as u64,
    });
    let slot_off = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Mul {
        dst: slot_off,
        lhs: slot_idx,
        rhs: slot_size_v,
    });

    let writing = ctx.alloc_vreg(SpillDesc::remat(RUNTIME_EVENT_WRITING));
    block.push(MInst::LoadImm {
        dst: writing,
        value: RUNTIME_EVENT_WRITING,
    });
    let slot_base = RUNTIME_EVENT_HEADER_SIZE as i32;
    block.push(MInst::ReleaseStorePtrIndexed {
        ptr: event_ptr,
        offset: slot_base + RUNTIME_EVENT_SLOT_SEQ_OFFSET as i32,
        index: slot_off,
        src: writing,
        size: OpSize::S64,
    });
    let site_v = ctx.alloc_vreg(SpillDesc::remat(site_id as u64));
    block.push(MInst::LoadImm {
        dst: site_v,
        value: site_id as u64,
    });
    block.push(MInst::StorePtrIndexed {
        ptr: event_ptr,
        offset: slot_base + RUNTIME_EVENT_SLOT_SITE_OFFSET as i32,
        index: slot_off,
        src: site_v,
        size: OpSize::S64,
    });
    let site_layout = &ctx.layout.runtime_event_site_layouts[site_id as usize];
    let arg_count = args.len() as u64;
    let arg_count_v = ctx.alloc_vreg(SpillDesc::remat(arg_count));
    block.push(MInst::LoadImm {
        dst: arg_count_v,
        value: arg_count,
    });
    block.push(MInst::StorePtrIndexed {
        ptr: event_ptr,
        offset: slot_base + RUNTIME_EVENT_SLOT_ARG_COUNT_OFFSET as i32,
        index: slot_off,
        src: arg_count_v,
        size: OpSize::S64,
    });
    for (idx, arg) in args.iter().enumerate() {
        let Some(arg_layout) = site_layout.args.get(idx) else {
            continue;
        };
        let value_chunks = if ctx.wide_regs.contains_key(arg) {
            ctx.get_wide_chunks(arg, block)
        } else {
            vec![(ctx.reg_map.get(*arg), ctx.sir_width(arg).min(64))]
        };
        let mask_chunks = if ctx.wide_regs.contains_key(arg) {
            get_wide_mask_chunks(ctx, block, arg, arg_layout.word_count)
        } else {
            vec![ctx.get_mask(*arg, block)]
        };
        for word_idx in 0..arg_layout.word_count {
            let value_vreg = value_chunks
                .get(word_idx)
                .map(|chunk| chunk.0)
                .unwrap_or_else(|| {
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    zero
                });
            block.push(MInst::StorePtrIndexed {
                ptr: event_ptr,
                offset: slot_base
                    + (RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                        + (arg_layout.value_word_offset + word_idx) * 8)
                        as i32,
                index: slot_off,
                src: value_vreg,
                size: OpSize::S64,
            });

            let mask_vreg = mask_chunks.get(word_idx).copied().unwrap_or_else(|| {
                let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: zero,
                    value: 0,
                });
                zero
            });
            block.push(MInst::StorePtrIndexed {
                ptr: event_ptr,
                offset: slot_base
                    + (RUNTIME_EVENT_SLOT_PAYLOAD_OFFSET
                        + (arg_layout.mask_word_offset + word_idx) * 8)
                        as i32,
                index: slot_off,
                src: mask_vreg,
                size: OpSize::S64,
            });
        }
    }
    block.push(MInst::ReleaseStorePtrIndexed {
        ptr: event_ptr,
        offset: slot_base + RUNTIME_EVENT_SLOT_SEQ_OFFSET as i32,
        index: slot_off,
        src: seq_v,
        size: OpSize::S64,
    });
    let next_seq = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::AddImm {
        dst: next_seq,
        src: seq_v,
        imm: 1,
    });
    block.push(MInst::ReleaseStorePtr {
        ptr: event_ptr,
        offset: 0,
        src: next_seq,
        size: OpSize::S64,
    });
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

#[derive(Clone, Copy)]
struct SirUseSite {
    block: crate::ir::BlockId,
    inst_idx: Option<usize>,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExactSirConstant {
    value: u64,
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

fn exact_sir_constant(value: &crate::ir::SIRValue) -> Option<ExactSirConstant> {
    if value.mask != num_bigint::BigUint::ZERO {
        return None;
    }
    let digits = value.payload.to_u64_digits();
    let value = match digits.as_slice() {
        [] => 0,
        [value] => *value,
        _ => return None,
    };
    Some(ExactSirConstant { value })
}

fn collect_exact_sir_constants(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, ExactSirConstant> {
    let mut constants = HashMap::default();
    let mut ambiguous = HashSet::default();
    for block in eu.blocks.values() {
        for inst in &block.instructions {
            let SIRInstruction::Imm(dst, value) = inst else {
                continue;
            };
            let Some(value) = exact_sir_constant(value) else {
                continue;
            };
            if constants.insert(*dst, value).is_some() {
                ambiguous.insert(*dst);
            }
        }
    }
    for reg in ambiguous {
        constants.remove(&reg);
    }
    constants
}

fn find_dense_lookup_plans(
    block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
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
    block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
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
    block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
    register_types: &HashMap<RegisterId, RegisterType>,
    constants: &HashMap<RegisterId, ExactSirConstant>,
    defs: &HashMap<RegisterId, usize>,
    cond: RegisterId,
) -> Option<DenseLookupCondition> {
    let mut cursor = cond;
    let mut covered_indices = HashSet::default();
    loop {
        let Some(&idx) = defs.get(&cursor) else {
            break;
        };
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

fn collect_sir_use_sites(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, Vec<SirUseSite>> {
    let mut uses: HashMap<RegisterId, Vec<SirUseSite>> = HashMap::default();
    for (block_id, block) in &eu.blocks {
        for (inst_idx, inst) in block.instructions.iter().enumerate() {
            collect_sir_inst_uses(inst, |reg| {
                uses.entry(reg).or_default().push(SirUseSite {
                    block: *block_id,
                    inst_idx: Some(inst_idx),
                });
            });
        }
        collect_sir_term_uses(&block.terminator, |reg| {
            uses.entry(reg).or_default().push(SirUseSite {
                block: *block_id,
                inst_idx: None,
            });
        });
    }
    uses
}

fn collect_sir_inst_uses(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    mut add: impl FnMut(RegisterId),
) {
    match inst {
        SIRInstruction::Imm(..) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            add(*lhs);
            add(*rhs);
        }
        SIRInstruction::Unary(_, _, src) | SIRInstruction::Slice(_, src, _, _) => add(*src),
        SIRInstruction::Load(_, _, SIROffset::Dynamic(off), _) => add(*off),
        SIRInstruction::Load(_, _, SIROffset::Static(_), _) => {}
        SIRInstruction::Store(_, off, _, src, _, _) => {
            if let SIROffset::Dynamic(off) = off {
                add(*off);
            }
            add(*src);
        }
        SIRInstruction::Commit(_, _, SIROffset::Dynamic(off), _, _) => add(*off),
        SIRInstruction::Commit(_, _, SIROffset::Static(_), _, _) => {}
        SIRInstruction::Concat(_, args)
        | SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => {
            for &arg in args {
                add(arg);
            }
        }
        SIRInstruction::Mux(_, cond, then_val, else_val) => {
            add(*cond);
            add(*then_val);
            add(*else_val);
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            add(*old);
            add(*new);
        }
    }
}

fn collect_sir_defs(
    block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
) -> HashMap<RegisterId, usize> {
    let mut defs = HashMap::default();
    for (idx, inst) in block.instructions.iter().enumerate() {
        if let Some(dst) = sir_def_reg(inst) {
            defs.insert(dst, idx);
        }
    }
    defs
}

fn collect_sir_term_uses(term: &SIRTerminator, mut add: impl FnMut(RegisterId)) {
    match term {
        SIRTerminator::Jump(_, args) => {
            for &arg in args {
                add(arg);
            }
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            add(*cond);
            for &arg in &true_block.1 {
                add(arg);
            }
            for &arg in &false_block.1 {
                add(arg);
            }
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

fn find_priority_encode_plans(
    block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
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
    block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
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
    block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
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
    block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
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
    block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
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
    block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
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
    block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
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

fn sir_def_reg(inst: &SIRInstruction<RegionedAbsoluteAddr>) -> Option<RegisterId> {
    match inst {
        SIRInstruction::Imm(dst, _)
        | SIRInstruction::Binary(dst, _, _, _)
        | SIRInstruction::Unary(dst, _, _)
        | SIRInstruction::Load(dst, _, _, _)
        | SIRInstruction::Concat(dst, _)
        | SIRInstruction::Slice(dst, _, _, _)
        | SIRInstruction::Mux(dst, _, _, _) => Some(*dst),
        SIRInstruction::Store(_, _, _, _, _, _)
        | SIRInstruction::Commit(_, _, _, _, _)
        | SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => None,
    }
}

fn def_used_only_by_candidate(
    block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
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

/// Lower the SystemVerilog truth state of a mux condition.
///
/// A vector condition is definitely true when at least one *known* bit is one.
/// It is unknown when there is no known-one bit and at least one X/Z bit; all
/// remaining conditions are definitely false.  Both returned VRegs are 0/1.
fn lower_mux_condition_state(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    cond: RegisterId,
) -> (VReg, VReg) {
    let width = ctx.sir_width(&cond);
    let n_chunks = ISelContext::num_chunks(width).max(1);
    let value_chunks = ctx.get_wide_chunks(&cond, block);
    let mask_chunks = if ctx.four_state {
        get_wide_mask_chunks(ctx, block, &cond, n_chunks)
    } else {
        Vec::new()
    };

    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let mut known_one_bits = zero;
    let mut unknown_bits = zero;

    for index in 0..n_chunks {
        let chunk_width = (width.saturating_sub(index * 64)).min(64);
        let value = value_chunks.get(index).map(|chunk| chunk.0).unwrap_or(zero);
        let mask = mask_chunks.get(index).copied().unwrap_or(zero);
        let (value, mask) = if chunk_width < 64 {
            let valid = mask_for_width(chunk_width);
            let masked_value = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked_value, value, valid);
            let masked_mask = if ctx.four_state {
                let masked_mask = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked_mask, mask, valid);
                masked_mask
            } else {
                zero
            };
            (masked_value, masked_mask)
        } else {
            (value, mask)
        };

        let known_ones = if ctx.four_state {
            let not_mask = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_mask,
                src: mask,
            });
            let known_ones = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: known_ones,
                lhs: value,
                rhs: not_mask,
            });
            known_ones
        } else {
            value
        };
        let next_known = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: next_known,
            lhs: known_one_bits,
            rhs: known_ones,
        });
        known_one_bits = next_known;

        if ctx.four_state {
            let next_unknown = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: next_unknown,
                lhs: unknown_bits,
                rhs: mask,
            });
            unknown_bits = next_unknown;
        }
    }

    let is_true = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: is_true,
        lhs: known_one_bits,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    ctx.known_bits.insert(is_true, 1);

    let is_unknown = if ctx.four_state {
        let has_unknown = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: has_unknown,
            lhs: unknown_bits,
            rhs: zero,
            kind: CmpKind::Ne,
        });
        let is_not_true = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: is_not_true,
            lhs: is_true,
            rhs: zero,
            kind: CmpKind::Eq,
        });
        let is_unknown = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: is_unknown,
            lhs: has_unknown,
            rhs: is_not_true,
        });
        ctx.known_bits.insert(is_unknown, 1);
        is_unknown
    } else {
        zero
    };

    (is_true, is_unknown)
}

/// Merge one result chunk for an unknown four-state mux condition.
/// Identical 4-state bits are preserved; every differing bit becomes X.
fn lower_four_state_mux_chunk(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    cond_is_true: VReg,
    cond_is_unknown: VReg,
    then_value: VReg,
    then_mask: VReg,
    else_value: VReg,
    else_mask: VReg,
    width: usize,
) -> (VReg, VReg) {
    let selected_value = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: selected_value,
        cond: cond_is_true,
        true_val: then_value,
        false_val: else_value,
    });
    let selected_mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: selected_mask,
        cond: cond_is_true,
        true_val: then_mask,
        false_val: else_mask,
    });

    let value_diff = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Xor {
        dst: value_diff,
        lhs: then_value,
        rhs: else_value,
    });
    let mask_diff = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Xor {
        dst: mask_diff,
        lhs: then_mask,
        rhs: else_mask,
    });
    let diff = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: diff,
        lhs: value_diff,
        rhs: mask_diff,
    });
    let unknown_value = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: unknown_value,
        lhs: then_value,
        rhs: diff,
    });
    let unknown_mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: unknown_mask,
        lhs: then_mask,
        rhs: diff,
    });

    let value = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: value,
        cond: cond_is_unknown,
        true_val: unknown_value,
        false_val: selected_value,
    });
    let mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: mask,
        cond: cond_is_unknown,
        true_val: unknown_mask,
        false_val: selected_mask,
    });

    if width < 64 {
        let logical_mask = mask_for_width(width);
        let masked_value = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, masked_value, value, logical_mask);
        let masked_mask = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, masked_mask, mask, logical_mask);
        (masked_value, masked_mask)
    } else {
        (value, mask)
    }
}

fn prepare_sparse_store(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    addr: &RegionedAbsoluteAddr,
    offset: &SIROffset,
    width: usize,
) {
    let abs = addr.absolute_addr();
    let sparse = &ctx.layout.sparse_layouts[&abs];
    let stable_base = ctx.layout.offsets[&abs] as i32;
    let sparse_base = (ctx.layout.sparse_base_offset + ctx.layout.sparse_offsets[&abs]) as i32;
    let byte_size = crate::backend::get_byte_size(ctx.layout.widths[&abs]) as i32;

    let bit_offset = match offset {
        SIROffset::Static(value) => {
            let reg = ctx.alloc_vreg(SpillDesc::remat(*value as u64));
            block.push(MInst::LoadImm {
                dst: reg,
                value: *value as u64,
            });
            reg
        }
        SIROffset::Dynamic(reg) => ctx.reg_map.get(*reg),
    };
    let start_chunk = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShrImm {
        dst: start_chunk,
        src: bit_offset,
        imm: 6,
    });
    let width_minus_one = ctx.alloc_vreg(SpillDesc::remat(width.saturating_sub(1) as u64));
    block.push(MInst::LoadImm {
        dst: width_minus_one,
        value: width.saturating_sub(1) as u64,
    });
    let end_bit = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Add {
        dst: end_bit,
        lhs: bit_offset,
        rhs: width_minus_one,
    });
    let end_chunk = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShrImm {
        dst: end_chunk,
        src: end_bit,
        imm: 6,
    });

    let max_chunks = match offset {
        SIROffset::Static(value) => ((value % 64) + width).div_ceil(64),
        SIROffset::Dynamic(reg) => {
            let zero_bits = ctx.low_zero_bits.get(reg).copied().unwrap_or(0).min(6);
            let alignment = 1usize << zero_bits;
            (width + (64 - alignment)).div_ceil(64)
        }
    };
    for chunk_delta in 0..max_chunks {
        let delta = ctx.alloc_vreg(SpillDesc::remat(chunk_delta as u64));
        block.push(MInst::LoadImm {
            dst: delta,
            value: chunk_delta as u64,
        });
        let candidate = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Add {
            dst: candidate,
            lhs: start_chunk,
            rhs: delta,
        });
        let valid = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: valid,
            lhs: candidate,
            rhs: end_chunk,
            kind: CmpKind::LeU,
        });
        let chunk = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Select {
            dst: chunk,
            cond: valid,
            true_val: candidate,
            false_val: start_chunk,
        });

        let dirty_word = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::ShrImm {
            dst: dirty_word,
            src: chunk,
            imm: 6,
        });
        let eight = ctx.alloc_vreg(SpillDesc::remat(8));
        block.push(MInst::LoadImm {
            dst: eight,
            value: 8,
        });
        let dirty_index = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Mul {
            dst: dirty_index,
            lhs: dirty_word,
            rhs: eight,
        });
        let dirty_bits = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::LoadIndexed {
            dst: dirty_bits,
            base: BaseReg::SimState,
            offset: sparse.dirty_words_offset as i32,
            index: dirty_index,
            size: OpSize::S64,
        });
        let bit_in_word = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, bit_in_word, chunk, 63);
        let one = ctx.alloc_vreg(SpillDesc::remat(1));
        block.push(MInst::LoadImm { dst: one, value: 1 });
        let dirty_mask = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Shl {
            dst: dirty_mask,
            lhs: one,
            rhs: bit_in_word,
        });
        let dirty_test = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: dirty_test,
            lhs: dirty_bits,
            rhs: dirty_mask,
        });
        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm {
            dst: zero,
            value: 0,
        });
        let was_dirty = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: was_dirty,
            lhs: dirty_test,
            rhs: zero,
            kind: CmpKind::Ne,
        });

        let data_index = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Mul {
            dst: data_index,
            lhs: chunk,
            rhs: eight,
        });
        for plane_delta in
            [0, byte_size]
                .into_iter()
                .take(if ctx.is_4state_var(addr) { 2 } else { 1 })
        {
            let stable = ctx.alloc_vreg(SpillDesc::transient());
            let working = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::LoadIndexed {
                dst: stable,
                base: BaseReg::SimState,
                offset: stable_base + plane_delta,
                index: data_index,
                size: OpSize::S64,
            });
            block.push(MInst::LoadIndexed {
                dst: working,
                base: BaseReg::SimState,
                offset: sparse_base + plane_delta,
                index: data_index,
                size: OpSize::S64,
            });
            let initialized = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: initialized,
                cond: was_dirty,
                true_val: working,
                false_val: stable,
            });
            block.push(MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset: sparse_base + plane_delta,
                index: data_index,
                src: initialized,
                size: OpSize::S64,
            });
        }

        let new_dirty = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: new_dirty,
            lhs: dirty_bits,
            rhs: dirty_mask,
        });
        block.push(MInst::StoreIndexed {
            base: BaseReg::SimState,
            offset: sparse.dirty_words_offset as i32,
            index: dirty_index,
            src: new_dirty,
            size: OpSize::S64,
        });

        let summary_word = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::ShrImm {
            dst: summary_word,
            src: dirty_word,
            imm: 6,
        });
        let summary_index = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Mul {
            dst: summary_index,
            lhs: summary_word,
            rhs: eight,
        });
        let summary_bits = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::LoadIndexed {
            dst: summary_bits,
            base: BaseReg::SimState,
            offset: sparse.summary_words_offset as i32,
            index: summary_index,
            size: OpSize::S64,
        });
        let summary_bit = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, summary_bit, dirty_word, 63);
        let summary_mask = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Shl {
            dst: summary_mask,
            lhs: one,
            rhs: summary_bit,
        });
        let new_summary = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: new_summary,
            lhs: summary_bits,
            rhs: summary_mask,
        });
        block.push(MInst::StoreIndexed {
            base: BaseReg::SimState,
            offset: sparse.summary_words_offset as i32,
            index: summary_index,
            src: new_summary,
            size: OpSize::S64,
        });
    }
}

fn emit_aligned_dynamic_wide_store(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    base_offset: i32,
    byte_offset: VReg,
    width: usize,
    chunks: &[(VReg, usize)],
) {
    let mut bit_pos = 0usize;
    let mut remaining = width;

    for &(chunk, chunk_width) in chunks {
        if remaining == 0 {
            break;
        }
        let logical_width = chunk_width.min(remaining);
        debug_assert!(bit_pos.is_multiple_of(8));

        let whole_bytes = logical_width / 8;
        let mut copied = 0usize;
        for bytes in [8usize, 4, 2, 1] {
            while copied + bytes <= whole_bytes {
                let consumed_bits = copied * 8;
                let src = if consumed_bits == 0 {
                    chunk
                } else {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: shifted,
                        src: chunk,
                        imm: consumed_bits as u8,
                    });
                    shifted
                };
                block.push(MInst::StoreIndexed {
                    base: BaseReg::SimState,
                    offset: base_offset + ((bit_pos / 8) + copied) as i32,
                    index: byte_offset,
                    src,
                    size: match bytes {
                        8 => OpSize::S64,
                        4 => OpSize::S32,
                        2 => OpSize::S16,
                        1 => OpSize::S8,
                        _ => unreachable!(),
                    },
                });
                copied += bytes;
            }
        }

        let tail_bits = logical_width % 8;
        if tail_bits != 0 {
            let consumed_bits = whole_bytes * 8;
            let src = if consumed_bits == 0 {
                chunk
            } else {
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: shifted,
                    src: chunk,
                    imm: consumed_bits as u8,
                });
                shifted
            };
            let offset = base_offset + ((bit_pos / 8) + whole_bytes) as i32;
            let old = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::LoadIndexed {
                dst: old,
                base: BaseReg::SimState,
                offset,
                index: byte_offset,
                size: OpSize::S8,
            });
            let new = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_bfi(block, new, old, src, 0, mask_for_width(tail_bits));
            block.push(MInst::StoreIndexed {
                base: BaseReg::SimState,
                offset,
                index: byte_offset,
                src: new,
                size: OpSize::S8,
            });
        }

        bit_pos += logical_width;
        remaining -= logical_width;
    }

    debug_assert_eq!(remaining, 0, "wide source does not cover store width");
}

fn emit_dynamic_scalar_bitfield_store(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    base_offset: i32,
    byte_offset: VReg,
    bit_shift: VReg,
    src: VReg,
    width: usize,
    track_change: bool,
) -> Option<VReg> {
    let width_mask = mask_for_width(width);
    let masked_src = ctx.alloc_vreg(SpillDesc::transient());
    if width_mask == u64::MAX {
        ctx.emit_mov(block, masked_src, src);
    } else {
        ctx.emit_and_imm(block, masked_src, src, width_mask);
    }

    let old_low = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::LoadIndexed {
        dst: old_low,
        base: BaseReg::SimState,
        offset: base_offset,
        index: byte_offset,
        size: ISelContext::op_size_for_width(width + 7),
    });
    let shifted_src = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Shl {
        dst: shifted_src,
        lhs: masked_src,
        rhs: bit_shift,
    });
    let mask_value = ctx.alloc_vreg(SpillDesc::remat(width_mask));
    block.push(MInst::LoadImm {
        dst: mask_value,
        value: width_mask,
    });
    let shifted_mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Shl {
        dst: shifted_mask,
        lhs: mask_value,
        rhs: bit_shift,
    });
    let inverted_mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::BitNot {
        dst: inverted_mask,
        src: shifted_mask,
    });
    let cleared_low = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::And {
        dst: cleared_low,
        lhs: old_low,
        rhs: inverted_mask,
    });
    let new_low = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: new_low,
        lhs: cleared_low,
        rhs: shifted_src,
    });
    block.push(MInst::StoreIndexed {
        base: BaseReg::SimState,
        offset: base_offset,
        index: byte_offset,
        src: new_low,
        size: ISelContext::op_size_for_width(width + 7),
    });

    let mut changed = track_change.then(|| {
        let changed = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: changed,
            lhs: old_low,
            rhs: new_low,
            kind: CmpKind::Ne,
        });
        changed
    });
    if width + 7 <= 64 {
        return changed;
    }

    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let sixty_four = ctx.alloc_vreg(SpillDesc::remat(64));
    block.push(MInst::LoadImm {
        dst: sixty_four,
        value: 64,
    });
    let inverse_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Sub {
        dst: inverse_shift,
        lhs: sixty_four,
        rhs: bit_shift,
    });
    let inverse_shift_mod = ctx.alloc_vreg(SpillDesc::transient());
    ctx.emit_and_imm(block, inverse_shift_mod, inverse_shift, 63);
    let has_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: has_shift,
        lhs: bit_shift,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    let high_src_raw = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Shr {
        dst: high_src_raw,
        lhs: masked_src,
        rhs: inverse_shift_mod,
    });
    let high_mask_raw = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Shr {
        dst: high_mask_raw,
        lhs: mask_value,
        rhs: inverse_shift_mod,
    });
    let high_src = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: high_src,
        cond: has_shift,
        true_val: high_src_raw,
        false_val: zero,
    });
    let high_mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: high_mask,
        cond: has_shift,
        true_val: high_mask_raw,
        false_val: zero,
    });
    let old_high = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::LoadIndexed {
        dst: old_high,
        base: BaseReg::SimState,
        offset: base_offset + 8,
        index: byte_offset,
        size: OpSize::S8,
    });
    let inverted_high_mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::BitNot {
        dst: inverted_high_mask,
        src: high_mask,
    });
    let cleared_high = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::And {
        dst: cleared_high,
        lhs: old_high,
        rhs: inverted_high_mask,
    });
    let new_high = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: new_high,
        lhs: cleared_high,
        rhs: high_src,
    });
    block.push(MInst::StoreIndexed {
        base: BaseReg::SimState,
        offset: base_offset + 8,
        index: byte_offset,
        src: new_high,
        size: OpSize::S8,
    });
    if track_change {
        let high_changed = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: high_changed,
            lhs: old_high,
            rhs: new_high,
            kind: CmpKind::Ne,
        });
        let any_changed = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: any_changed,
            lhs: changed.expect("low change was requested"),
            rhs: high_changed,
        });
        changed = Some(any_changed);
    }
    changed
}

fn emit_dynamic_wide_bitfield_store(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    base_offset: i32,
    byte_offset: VReg,
    bit_shift: VReg,
    width: usize,
    chunks: &[(VReg, usize)],
    track_change: bool,
) -> Option<VReg> {
    let mut remaining = width;
    let mut bit_pos = 0usize;
    let mut changed = None;
    for &(chunk, chunk_width) in chunks {
        if remaining == 0 {
            break;
        }
        let logical_width = chunk_width.min(remaining).min(64);
        let chunk_changed = emit_dynamic_scalar_bitfield_store(
            ctx,
            block,
            base_offset + (bit_pos / 8) as i32,
            byte_offset,
            bit_shift,
            chunk,
            logical_width,
            track_change,
        );
        changed = match (changed, chunk_changed) {
            (None, next) => next,
            (Some(previous), Some(next)) => {
                let merged = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: merged,
                    lhs: previous,
                    rhs: next,
                });
                Some(merged)
            }
            (previous, None) => previous,
        };
        bit_pos += logical_width;
        remaining -= logical_width;
    }
    changed
}

fn lower_instruction(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    sir_block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
    sir_defs: &HashMap<RegisterId, usize>,
) {
    if let SIRInstruction::Commit(src, dst, _, _, _) = inst
        && src.region == crate::ir::SPARSE_WORKING_REGION
        && dst.region == STABLE_REGION
    {
        let abs = src.absolute_addr();
        let sparse = &ctx.layout.sparse_layouts[&abs];
        block.push(MInst::SparseCommit {
            src_offset: (ctx.layout.sparse_base_offset + ctx.layout.sparse_offsets[&abs]) as i32,
            dst_offset: ctx.layout.offsets[&abs] as i32,
            byte_size: crate::backend::get_byte_size(ctx.layout.widths[&abs]),
            dirty_words_offset: sparse.dirty_words_offset as i32,
            dirty_word_count: sparse.dirty_word_count,
            summary_words_offset: sparse.summary_words_offset as i32,
            summary_word_count: sparse.summary_word_count,
            four_state: ctx.four_state && ctx.layout.is_4states[&abs],
        });
        return;
    }
    match inst {
        SIRInstruction::RuntimeEvent { site_id, args } => {
            let event_ptr = load_runtime_event_ptr(ctx, block);
            lower_runtime_event_write(ctx, block, event_ptr, *site_id, args);
        }
        SIRInstruction::CombCaptureEvent { .. } => {
            unreachable!("comb capture events are CFG-lowered by lower_execution_unit")
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, sites } => {
            emit_enable_comb_capture_sites_if_regs_changed(ctx, block, *old, *new, sites);
        }
        SIRInstruction::Mux(dst, cond, then_val, else_val) => {
            let d_width = ctx.sir_width(dst);
            let (cond_is_true, cond_is_unknown) = lower_mux_condition_state(ctx, block, *cond);

            if d_width > 64 {
                let n_chunks = ISelContext::num_chunks(d_width);
                let tv_chunks = ctx.get_wide_chunks(then_val, block);
                let ev_chunks = ctx.get_wide_chunks(else_val, block);
                let zero_v = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: zero_v,
                    value: 0,
                });
                if ctx.four_state {
                    let tm_chunks = get_wide_mask_chunks(ctx, block, then_val, n_chunks);
                    let em_chunks = get_wide_mask_chunks(ctx, block, else_val, n_chunks);
                    let mut value_chunks = Vec::with_capacity(n_chunks);
                    let mut mask_chunks = Vec::with_capacity(n_chunks);
                    for i in 0..n_chunks {
                        let tv = tv_chunks.get(i).map(|chunk| chunk.0).unwrap_or(zero_v);
                        let ev = ev_chunks.get(i).map(|chunk| chunk.0).unwrap_or(zero_v);
                        let tm = *tm_chunks.get(i).unwrap_or(&zero_v);
                        let em = *em_chunks.get(i).unwrap_or(&zero_v);
                        let chunk_width = (d_width - i * 64).min(64);
                        let (value, mask) = lower_four_state_mux_chunk(
                            ctx,
                            block,
                            cond_is_true,
                            cond_is_unknown,
                            tv,
                            tm,
                            ev,
                            em,
                            chunk_width,
                        );
                        value_chunks.push((value, chunk_width));
                        mask_chunks.push((mask, chunk_width));
                    }
                    ctx.set_wide_chunks(*dst, value_chunks);
                    ctx.set_mask(*dst, mask_chunks[0].0);
                    ctx.wide_masks.insert(*dst, mask_chunks);
                } else {
                    let mut value_chunks = Vec::with_capacity(n_chunks);
                    for i in 0..n_chunks {
                        let tv = tv_chunks.get(i).map(|chunk| chunk.0).unwrap_or(zero_v);
                        let ev = ev_chunks.get(i).map(|chunk| chunk.0).unwrap_or(zero_v);
                        let chunk_width = (d_width - i * 64).min(64);
                        let selected = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Select {
                            dst: selected,
                            cond: cond_is_true,
                            true_val: tv,
                            false_val: ev,
                        });
                        let value = if chunk_width < 64 {
                            let masked = ctx.alloc_vreg(SpillDesc::transient());
                            ctx.emit_and_imm(block, masked, selected, mask_for_width(chunk_width));
                            masked
                        } else {
                            selected
                        };
                        value_chunks.push((value, chunk_width));
                    }
                    ctx.set_wide_chunks(*dst, value_chunks);
                }
            } else {
                let dst_vreg = ctx.reg_map.get(*dst);
                let tv = if ctx.wide_regs.contains_key(then_val) {
                    ctx.get_wide_chunks(then_val, block)[0].0
                } else {
                    ctx.reg_map.get(*then_val)
                };
                let ev = if ctx.wide_regs.contains_key(else_val) {
                    ctx.get_wide_chunks(else_val, block)[0].0
                } else {
                    ctx.reg_map.get(*else_val)
                };

                if !ctx.four_state && d_width == 1 {
                    let tv = lower_low_bit(ctx, block, tv);
                    let ev = lower_low_bit(ctx, block, ev);

                    block.push(MInst::Select {
                        dst: dst_vreg,
                        cond: cond_is_true,
                        true_val: tv,
                        false_val: ev,
                    });
                    ctx.known_bits.insert(dst_vreg, 1);
                    return;
                }

                if !ctx.four_state
                    && d_width <= 64
                    && ctx.known_bits.get(&tv).copied().unwrap_or(64) <= d_width
                    && ctx.known_bits.get(&ev).copied().unwrap_or(64) <= d_width
                    && let Some((guard, lhs, rhs, kind)) =
                        match_guarded_cmp_select_cond(ctx, block, sir_block, sir_defs, *cond)
                {
                    block.push(MInst::GuardedCmpSelect {
                        dst: dst_vreg,
                        guard,
                        lhs,
                        rhs,
                        kind,
                        true_val: tv,
                        false_val: ev,
                    });
                    ctx.known_bits.insert(dst_vreg, d_width);
                    return;
                }

                if !ctx.four_state
                    && d_width <= 64
                    && ctx.known_bits.get(&tv).copied().unwrap_or(64) <= d_width
                    && ctx.known_bits.get(&ev).copied().unwrap_or(64) <= d_width
                {
                    block.push(MInst::Select {
                        dst: dst_vreg,
                        cond: cond_is_true,
                        true_val: tv,
                        false_val: ev,
                    });
                    ctx.known_bits.insert(dst_vreg, d_width);
                    return;
                }

                if ctx.four_state {
                    let tm = ctx.get_mask(*then_val, block);
                    let em = ctx.get_mask(*else_val, block);
                    let (value, mask) = lower_four_state_mux_chunk(
                        ctx,
                        block,
                        cond_is_true,
                        cond_is_unknown,
                        tv,
                        tm,
                        ev,
                        em,
                        d_width,
                    );
                    ctx.emit_mov(block, dst_vreg, value);
                    ctx.set_mask(*dst, mask);
                } else {
                    let selected = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: selected,
                        cond: cond_is_true,
                        true_val: tv,
                        false_val: ev,
                    });
                    if d_width < 64 {
                        ctx.emit_and_imm(block, dst_vreg, selected, mask_for_width(d_width));
                    } else {
                        ctx.emit_mov(block, dst_vreg, selected);
                    }
                }
            }
        }
        SIRInstruction::Imm(dst, val) => {
            let d_width = ctx.sir_width(dst);
            let digits = val.payload.to_u64_digits();
            let imm_val = digits.first().copied().unwrap_or(0);

            let vreg = ctx.reg_map.get(*dst);
            ctx.spill_descs[vreg.0 as usize] = SpillDesc::remat(imm_val);
            block.push(MInst::LoadImm {
                dst: vreg,
                value: imm_val,
            });
            // Track constant value for later folding
            ctx.consts.insert(*dst, imm_val);
            set_low_zero_bits(ctx, *dst, low_zero_bits_const(imm_val));
            // Known bits from the constant value
            let imm_bits = if imm_val == 0 {
                0
            } else {
                64 - imm_val.leading_zeros() as usize
            };
            ctx.known_bits.insert(vreg, imm_bits.min(d_width));

            // For wide values, also store chunks in wide_regs
            if d_width > 64 {
                let n_chunks = ISelContext::num_chunks(d_width);
                let mut chunks = Vec::with_capacity(n_chunks);
                for i in 0..n_chunks {
                    let chunk_val = digits.get(i).copied().unwrap_or(0);
                    if i == 0 {
                        chunks.push((vreg, 64));
                    } else {
                        let cv = ctx.alloc_vreg(SpillDesc::remat(chunk_val));
                        block.push(MInst::LoadImm {
                            dst: cv,
                            value: chunk_val,
                        });
                        chunks.push((cv, 64));
                    }
                }
                ctx.set_wide_chunks(*dst, chunks);
            }

            // 4-state: load mask immediate
            if ctx.four_state {
                let mask_digits = val.mask.to_u64_digits();
                let mask_val = mask_digits.first().copied().unwrap_or(0);
                let mvreg = ctx.alloc_vreg(SpillDesc::remat(mask_val));
                block.push(MInst::LoadImm {
                    dst: mvreg,
                    value: mask_val,
                });
                ctx.set_mask(*dst, mvreg);

                if d_width > 64 {
                    let n_chunks = ISelContext::num_chunks(d_width);
                    let mut mchunks = Vec::with_capacity(n_chunks);
                    mchunks.push((mvreg, 64));
                    for i in 1..n_chunks {
                        let cv = mask_digits.get(i).copied().unwrap_or(0);
                        let mv = ctx.alloc_vreg(SpillDesc::remat(cv));
                        block.push(MInst::LoadImm { dst: mv, value: cv });
                        mchunks.push((mv, 64));
                    }
                    ctx.wide_masks.insert(*dst, mchunks);
                }
            }
        }

        SIRInstruction::Load(dst, addr, offset, width_bits) => {
            match offset {
                SIROffset::Static(bit_offset) => {
                    ctx.reg_addrs.insert(*dst, (*addr, *bit_offset));
                }
                SIROffset::Dynamic(_) => {
                    ctx.reg_addrs.remove(dst);
                }
            }
            let vreg = ctx.reg_map.get(*dst);

            match offset {
                SIROffset::Static(bit_off) => {
                    let intra_byte = bit_off % 8;
                    let crosses_native_word = intra_byte + *width_bits > 64;
                    if intra_byte != 0 && (*width_bits > 64 || crosses_native_word) {
                        let value_base = ctx.byte_offset(addr, 0);
                        let chunks = lower_static_wide_load_chunks(
                            ctx,
                            block,
                            value_base,
                            *bit_off,
                            *width_bits,
                        );
                        ctx.emit_alias_mov(block, vreg, chunks[0].0);
                        if *width_bits > 64 {
                            ctx.wide_regs.insert(*dst, chunks);
                        }

                        if ctx.is_4state_var(addr) {
                            let mask_base = ctx.mask_byte_offset(addr, 0);
                            let mask_chunks = lower_static_wide_load_chunks(
                                ctx,
                                block,
                                mask_base,
                                *bit_off,
                                *width_bits,
                            );
                            ctx.set_mask(*dst, mask_chunks[0].0);
                            if *width_bits > 64 {
                                ctx.wide_masks.insert(*dst, mask_chunks);
                            }
                        } else if ctx.four_state {
                            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                            block.push(MInst::LoadImm {
                                dst: zero,
                                value: 0,
                            });
                            ctx.set_mask(*dst, zero);
                        }
                        return;
                    }
                    // Wide load (>64 bits): chunk-by-chunk
                    if *width_bits > 64 {
                        let n_chunks = ISelContext::num_chunks(*width_bits);
                        let mut chunks = Vec::with_capacity(n_chunks);
                        let mut remaining = *width_bits;
                        let mut bit_pos = *bit_off;
                        for _ in 0..n_chunks {
                            let chunk_bits = remaining.min(64);
                            let chunk_byte_off = ctx.byte_offset(addr, bit_pos);
                            let chunk_size = ISelContext::op_size_for_width(chunk_bits);
                            let chunk_vreg = ctx.alloc_vreg(SpillDesc::sim_state(
                                *addr, bit_pos, chunk_bits, false,
                            ));
                            block.push(MInst::Load {
                                dst: chunk_vreg,
                                base: BaseReg::SimState,
                                offset: chunk_byte_off,
                                size: chunk_size,
                            });
                            chunks.push((chunk_vreg, chunk_bits));
                            bit_pos += chunk_bits;
                            remaining -= chunk_bits;
                        }
                        // Also store chunk[0] in reg_map scalar slot for
                        // fallback paths that read the scalar VReg.
                        ctx.emit_alias_mov(block, vreg, chunks[0].0);
                        ctx.wide_regs.insert(*dst, chunks);

                        // 4-state: load wide mask chunks
                        if ctx.is_4state_var(addr) {
                            let mut mchunks = Vec::with_capacity(n_chunks);
                            let mut m_remaining = *width_bits;
                            let mut m_bit_pos = *bit_off;
                            for _ in 0..n_chunks {
                                let chunk_bits = m_remaining.min(64);
                                let chunk_byte_off = ctx.mask_byte_offset(addr, m_bit_pos);
                                let chunk_size = ISelContext::op_size_for_width(chunk_bits);
                                let mv = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Load {
                                    dst: mv,
                                    base: BaseReg::SimState,
                                    offset: chunk_byte_off,
                                    size: chunk_size,
                                });
                                mchunks.push((mv, chunk_bits));
                                m_bit_pos += chunk_bits;
                                m_remaining -= chunk_bits;
                            }
                            let mvreg = ctx.alloc_vreg(SpillDesc::transient());
                            ctx.emit_alias_mov(block, mvreg, mchunks[0].0);
                            ctx.set_mask(*dst, mvreg);
                            ctx.wide_masks.insert(*dst, mchunks);
                        } else if ctx.four_state {
                            let mvreg = ctx.alloc_vreg(SpillDesc::remat(0));
                            block.push(MInst::LoadImm {
                                dst: mvreg,
                                value: 0,
                            });
                            ctx.set_mask(*dst, mvreg);
                        }
                        return;
                    }

                    let byte_off = ctx.byte_offset(addr, *bit_off);
                    let op_size = ISelContext::op_size_for_width(*width_bits);

                    // Update spill desc
                    ctx.spill_descs[vreg.0 as usize] =
                        SpillDesc::sim_state(*addr, *bit_off, *width_bits, false);

                    if !ctx.four_state
                        && let Some(load_size) =
                            ctx.full_static_load_size(addr, *bit_off, *width_bits)
                    {
                        block.push(MInst::Load {
                            dst: vreg,
                            base: BaseReg::SimState,
                            offset: byte_off,
                            size: load_size,
                        });
                        ctx.known_bits.insert(vreg, *width_bits);
                    } else if intra_byte == 0 && OpSize::from_bits(*width_bits).is_some() {
                        // Word-aligned, native size: single load.
                        // If the load is wider than the variable (SIR optimizer widening),
                        // mask the result to the variable's actual width.
                        let var_width = ctx
                            .layout
                            .widths
                            .get(&addr.absolute_addr())
                            .copied()
                            .unwrap_or(*width_bits);
                        if var_width < *width_bits && var_width < 64 {
                            let raw = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Load {
                                dst: raw,
                                base: BaseReg::SimState,
                                offset: byte_off,
                                size: op_size,
                            });
                            ctx.emit_and_imm(block, vreg, raw, mask_for_width(var_width));
                        } else {
                            block.push(MInst::Load {
                                dst: vreg,
                                base: BaseReg::SimState,
                                offset: byte_off,
                                size: op_size,
                            });
                        }
                    } else {
                        // Unaligned or non-native width: load containing word + shift + mask
                        let containing_byte_off = ctx.byte_offset(addr, 0) + (bit_off / 8) as i32;
                        let load_size = ISelContext::op_size_for_width(*width_bits + intra_byte);

                        let tmp = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Load {
                            dst: tmp,
                            base: BaseReg::SimState,
                            offset: containing_byte_off,
                            size: load_size,
                        });

                        if intra_byte > 0 {
                            let shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShrImm {
                                dst: shifted,
                                src: tmp,
                                imm: intra_byte as u8,
                            });
                            let mask = mask_for_width(*width_bits);
                            ctx.emit_and_imm(block, vreg, shifted, mask);
                        } else {
                            // Byte-aligned but non-native width: just mask
                            let mask = mask_for_width(*width_bits);
                            ctx.emit_and_imm(block, vreg, tmp, mask);
                        }
                    }
                }
                SIROffset::Dynamic(offset_reg) => {
                    // Dynamic offset: offset_reg holds total bit offset.
                    // byte_offset = total_bit_offset >> 3
                    // bit_shift = total_bit_offset & 7
                    let offset_vreg = ctx.reg_map.get(*offset_reg);
                    let base_off = ctx.byte_offset(addr, 0);

                    // Compute byte offset and intra-byte bit shift
                    let byte_off = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: byte_off,
                        src: offset_vreg,
                        imm: 3,
                    });
                    let may_cross_native_word =
                        *width_bits + 7 > 64 && low_zero_bits_reg(ctx, *offset_reg) < 3;
                    if *width_bits > 64 || may_cross_native_word {
                        let chunks = lower_dynamic_wide_load_chunks(
                            ctx,
                            block,
                            base_off,
                            byte_off,
                            offset_vreg,
                            *offset_reg,
                            *width_bits,
                        );
                        if *width_bits > 64 {
                            ctx.set_wide_chunks(*dst, chunks);
                        } else {
                            ctx.emit_alias_mov(block, vreg, chunks[0].0);
                        }

                        if ctx.is_4state_var(addr) {
                            let mask_base_off = ctx.mask_byte_offset(addr, 0);
                            let mask_chunks = lower_dynamic_wide_load_chunks(
                                ctx,
                                block,
                                mask_base_off,
                                byte_off,
                                offset_vreg,
                                *offset_reg,
                                *width_bits,
                            );
                            if let Some(&(mask0, _)) = mask_chunks.first() {
                                ctx.set_mask(*dst, mask0);
                            }
                            if *width_bits > 64 {
                                ctx.wide_masks.insert(*dst, mask_chunks);
                            }
                        } else if ctx.four_state {
                            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                            block.push(MInst::LoadImm {
                                dst: zero,
                                value: 0,
                            });
                            ctx.set_mask(*dst, zero);
                        }
                        return;
                    }
                    if low_zero_bits_reg(ctx, *offset_reg) >= 3 {
                        let load_size = ISelContext::op_size_for_width(*width_bits);
                        let raw = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::LoadIndexed {
                            dst: raw,
                            base: BaseReg::SimState,
                            offset: base_off,
                            index: byte_off,
                            size: load_size,
                        });
                        if *width_bits < 64 {
                            ctx.emit_and_imm(block, vreg, raw, mask_for_width(*width_bits));
                        } else {
                            ctx.emit_mov(block, vreg, raw);
                        }
                    } else {
                        let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
                        ctx.emit_and_imm(block, bit_shift, offset_vreg, 7);

                        // Load containing word at [sim + base_off + byte_off]
                        // Use a slightly larger load to account for bit_shift
                        let load_size = ISelContext::op_size_for_width(*width_bits + 7);
                        let raw = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::LoadIndexed {
                            dst: raw,
                            base: BaseReg::SimState,
                            offset: base_off,
                            index: byte_off,
                            size: load_size,
                        });

                        // Shift right by bit_shift (dynamic)
                        let shifted = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Shr {
                            dst: shifted,
                            lhs: raw,
                            rhs: bit_shift,
                        });

                        // Mask to width
                        if *width_bits < 64 {
                            let mask = mask_for_width(*width_bits);
                            ctx.emit_and_imm(block, vreg, shifted, mask);
                        } else {
                            ctx.emit_mov(block, vreg, shifted);
                        }
                    }
                }
            }

            // 4-state: load mask from memory (narrow path; wide handled above in early-return)
            if ctx.is_4state_var(addr) {
                match offset {
                    SIROffset::Static(bit_off) => {
                        if *width_bits <= 64 {
                            let mask_off = ctx.mask_byte_offset(addr, *bit_off);
                            let intra_byte = bit_off % 8;
                            let op_size = ISelContext::op_size_for_width(*width_bits);
                            let mvreg = ctx.alloc_vreg(SpillDesc::transient());
                            let var_width = ctx
                                .layout
                                .widths
                                .get(&addr.absolute_addr())
                                .copied()
                                .unwrap_or(*width_bits);

                            if intra_byte == 0 && OpSize::from_bits(*width_bits).is_some() {
                                // Mask to actual variable width if SIR optimizer widened the load
                                if var_width < *width_bits && var_width < 64 {
                                    let raw = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::Load {
                                        dst: raw,
                                        base: BaseReg::SimState,
                                        offset: mask_off,
                                        size: op_size,
                                    });
                                    ctx.emit_and_imm(block, mvreg, raw, mask_for_width(var_width));
                                } else {
                                    block.push(MInst::Load {
                                        dst: mvreg,
                                        base: BaseReg::SimState,
                                        offset: mask_off,
                                        size: op_size,
                                    });
                                }
                            } else {
                                let containing_off =
                                    ctx.mask_byte_offset(addr, 0) + (bit_off / 8) as i32;
                                let load_size =
                                    ISelContext::op_size_for_width(*width_bits + intra_byte);
                                let tmp = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Load {
                                    dst: tmp,
                                    base: BaseReg::SimState,
                                    offset: containing_off,
                                    size: load_size,
                                });
                                if intra_byte > 0 {
                                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::ShrImm {
                                        dst: shifted,
                                        src: tmp,
                                        imm: intra_byte as u8,
                                    });
                                    ctx.emit_and_imm(
                                        block,
                                        mvreg,
                                        shifted,
                                        mask_for_width(*width_bits),
                                    );
                                } else {
                                    ctx.emit_and_imm(
                                        block,
                                        mvreg,
                                        tmp,
                                        mask_for_width(*width_bits),
                                    );
                                }
                            }
                            ctx.set_mask(*dst, mvreg);
                        }
                    }
                    SIROffset::Dynamic(offset_reg) => {
                        // Dynamic: load mask similarly with indexed addressing
                        let offset_vreg = ctx.reg_map.get(*offset_reg);
                        let mask_base_off = ctx.mask_byte_offset(addr, 0);
                        let byte_off = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShrImm {
                            dst: byte_off,
                            src: offset_vreg,
                            imm: 3,
                        });
                        if low_zero_bits_reg(ctx, *offset_reg) >= 3 {
                            let load_size = ISelContext::op_size_for_width(*width_bits);
                            let raw = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::LoadIndexed {
                                dst: raw,
                                base: BaseReg::SimState,
                                offset: mask_base_off,
                                index: byte_off,
                                size: load_size,
                            });
                            let mvreg = ctx.alloc_vreg(SpillDesc::transient());
                            if *width_bits < 64 {
                                ctx.emit_and_imm(block, mvreg, raw, mask_for_width(*width_bits));
                            } else {
                                ctx.emit_mov(block, mvreg, raw);
                            }
                            ctx.set_mask(*dst, mvreg);
                            return;
                        }
                        let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
                        ctx.emit_and_imm(block, bit_shift, offset_vreg, 7);
                        let load_size = ISelContext::op_size_for_width(*width_bits + 7);
                        let raw = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::LoadIndexed {
                            dst: raw,
                            base: BaseReg::SimState,
                            offset: mask_base_off,
                            index: byte_off,
                            size: load_size,
                        });
                        let shifted = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Shr {
                            dst: shifted,
                            lhs: raw,
                            rhs: bit_shift,
                        });
                        let mvreg = ctx.alloc_vreg(SpillDesc::transient());
                        if *width_bits < 64 {
                            ctx.emit_and_imm(block, mvreg, shifted, mask_for_width(*width_bits));
                        } else {
                            ctx.emit_mov(block, mvreg, shifted);
                        }
                        ctx.set_mask(*dst, mvreg);
                    }
                }
            } else if ctx.four_state {
                // Non-4-state variable in 4-state mode: mask is always 0
                let mvreg = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: mvreg,
                    value: 0,
                });
                ctx.set_mask(*dst, mvreg);
            }
        }

        SIRInstruction::Store(addr, offset, width_bits, src_reg, triggers, comb_capture_sites) => {
            if addr.region == crate::ir::SPARSE_WORKING_REGION && *width_bits != 0 {
                prepare_sparse_store(ctx, block, addr, offset, *width_bits);
            }
            // width=0: identity Store optimized away; only emit triggers.
            if *width_bits == 0 {
                if !triggers.is_empty() {
                    if let SIROffset::Static(bit_off) = offset {
                        // Load current value for trigger comparison
                        // (self-copy alias: addr points to the canonical location)
                        let byte_off = ctx.byte_offset(addr, *bit_off);
                        let triggers = triggers
                            .iter()
                            .copied()
                            .filter(|trigger| ctx.trigger_only_seen.insert((byte_off, trigger.id)))
                            .collect::<Vec<_>>();
                        if triggers.is_empty() {
                            return;
                        }
                        // We need *some* value for trigger comparison.
                        // Since width was originally 1 (clock/reset), load 1 byte.
                        let new_val = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Load {
                            dst: new_val,
                            base: BaseReg::SimState,
                            offset: byte_off,
                            size: OpSize::S8,
                        });

                        for trigger in &triggers {
                            let trigger_byte_idx = trigger.id / 8;
                            let trigger_bit_idx = trigger.id % 8;
                            let trigger_offset =
                                ctx.layout.triggered_bits_offset + trigger_byte_idx;

                            let triggered = ctx.alloc_vreg(SpillDesc::transient());
                            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                            block.push(MInst::LoadImm {
                                dst: zero,
                                value: 0,
                            });
                            block.push(MInst::Cmp {
                                dst: triggered,
                                lhs: new_val,
                                rhs: zero,
                                kind: CmpKind::Ne,
                            });

                            let old_byte = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Load {
                                dst: old_byte,
                                base: BaseReg::SimState,
                                offset: trigger_offset as i32,
                                size: OpSize::S8,
                            });
                            let bit_mask =
                                ctx.alloc_vreg(SpillDesc::remat(1u64 << trigger_bit_idx));
                            block.push(MInst::LoadImm {
                                dst: bit_mask,
                                value: 1u64 << trigger_bit_idx,
                            });
                            let selected_mask = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Select {
                                dst: selected_mask,
                                cond: triggered,
                                true_val: bit_mask,
                                false_val: zero,
                            });
                            let new_byte = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or {
                                dst: new_byte,
                                lhs: old_byte,
                                rhs: selected_mask,
                            });
                            block.push(MInst::Store {
                                base: BaseReg::SimState,
                                offset: trigger_offset as i32,
                                src: new_byte,
                                size: OpSize::S8,
                            });
                        }
                    }
                }
                // Skip value store entirely
            } else {
                ctx.trigger_only_seen.clear();
                let old_comb_probe = if comb_capture_sites.is_empty() || *width_bits > 64 {
                    None
                } else {
                    match offset {
                        SIROffset::Static(bit_off) => {
                            let intra = bit_off % 8;
                            let containing_byte_off =
                                ctx.byte_offset(addr, 0) + (bit_off / 8) as i32;
                            let size = ISelContext::op_size_for_width(*width_bits + intra);
                            let old = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Load {
                                dst: old,
                                base: BaseReg::SimState,
                                offset: containing_byte_off,
                                size,
                            });
                            Some((old, containing_byte_off, size))
                        }
                        SIROffset::Dynamic(_) => None,
                    }
                };
                let old_comb_wide_probe = if comb_capture_sites.is_empty() || *width_bits <= 64 {
                    Vec::new()
                } else if let SIROffset::Static(bit_off) = offset {
                    collect_static_comb_store_byte_probes(
                        ctx,
                        block,
                        addr,
                        *bit_off,
                        *width_bits,
                        false,
                    )
                } else {
                    Vec::new()
                };
                let old_comb_mask_probe = if comb_capture_sites.is_empty()
                    || *width_bits > 64
                    || !ctx.is_4state_var(addr)
                {
                    None
                } else {
                    match offset {
                        SIROffset::Static(bit_off) => {
                            let intra = bit_off % 8;
                            let containing_byte_off =
                                ctx.mask_byte_offset(addr, 0) + (bit_off / 8) as i32;
                            let size = ISelContext::op_size_for_width(*width_bits + intra);
                            let old = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Load {
                                dst: old,
                                base: BaseReg::SimState,
                                offset: containing_byte_off,
                                size,
                            });
                            Some((old, containing_byte_off, size))
                        }
                        SIROffset::Dynamic(_) => None,
                    }
                };
                let old_comb_wide_mask_probe = if comb_capture_sites.is_empty()
                    || *width_bits <= 64
                    || !ctx.is_4state_var(addr)
                {
                    Vec::new()
                } else if let SIROffset::Static(bit_off) = offset {
                    collect_static_comb_store_byte_probes(
                        ctx,
                        block,
                        addr,
                        *bit_off,
                        *width_bits,
                        true,
                    )
                } else {
                    Vec::new()
                };
                match offset {
                    SIROffset::Static(bit_off) => {
                        // Check for wide value from Concat
                        if *width_bits > 64 {
                            if let Some(chunks) = ctx.wide_regs.get(src_reg).cloned() {
                                // Wide store: emit chunk-by-chunk stores
                                let mut bit_pos = 0usize;
                                let mut store_remaining = *width_bits;
                                for (chunk_vreg, chunk_width) in &chunks {
                                    if store_remaining == 0 {
                                        break;
                                    }
                                    let logical_chunk_width = (*chunk_width).min(store_remaining);
                                    let mut consumed = 0usize;
                                    while consumed < logical_chunk_width {
                                        let part_bit_off = *bit_off + bit_pos + consumed;
                                        let intra = part_bit_off % 8;
                                        let remaining = logical_chunk_width - consumed;
                                        let part_width = remaining.min(64 - intra);
                                        let part_byte_off = ctx.byte_offset(addr, part_bit_off);

                                        let part_src = if consumed == 0 {
                                            *chunk_vreg
                                        } else {
                                            let shifted = ctx.alloc_vreg(SpillDesc::transient());
                                            block.push(MInst::ShrImm {
                                                dst: shifted,
                                                src: *chunk_vreg,
                                                imm: consumed as u8,
                                            });
                                            shifted
                                        };

                                        if intra == 0 && OpSize::from_bits(part_width).is_some() {
                                            block.push(MInst::Store {
                                                base: BaseReg::SimState,
                                                offset: part_byte_off,
                                                src: part_src,
                                                size: OpSize::from_bits(part_width).unwrap(),
                                            });
                                        } else {
                                            // Non-aligned chunk part: RMW via BitFieldInsert.
                                            let containing_off = ctx.byte_offset(addr, 0)
                                                + (part_bit_off / 8) as i32;
                                            let load_size =
                                                ISelContext::op_size_for_width(part_width + intra);
                                            let old = ctx.alloc_vreg(SpillDesc::transient());
                                            block.push(MInst::Load {
                                                dst: old,
                                                base: BaseReg::SimState,
                                                offset: containing_off,
                                                size: load_size,
                                            });
                                            let mask = mask_for_width(part_width);
                                            let new = ctx.alloc_vreg(SpillDesc::transient());
                                            ctx.emit_bfi(
                                                block,
                                                new,
                                                old,
                                                part_src,
                                                intra as u8,
                                                mask,
                                            );
                                            block.push(MInst::Store {
                                                base: BaseReg::SimState,
                                                offset: containing_off,
                                                src: new,
                                                size: load_size,
                                            });
                                        }
                                        consumed += part_width;
                                    }
                                    bit_pos += logical_chunk_width;
                                    store_remaining -= logical_chunk_width;
                                }
                            } else {
                                // Wide store without Concat source: chunk-by-chunk copy
                                // This shouldn't happen in practice since wide stores
                                // come from Concat, but handle it as raw memory copy.
                                let mut remaining = *width_bits;
                                let mut off = ctx.byte_offset(addr, *bit_off);
                                while remaining > 0 {
                                    let chunk_bits = remaining.min(64);
                                    let chunk_size = ISelContext::op_size_for_width(chunk_bits);
                                    let tmp = ctx.alloc_vreg(SpillDesc::transient());
                                    // We can't split a single vreg. This is a fallback.
                                    block.push(MInst::LoadImm { dst: tmp, value: 0 });
                                    block.push(MInst::Store {
                                        base: BaseReg::SimState,
                                        offset: off,
                                        src: tmp,
                                        size: chunk_size,
                                    });
                                    let advance = chunk_bits.div_ceil(8);
                                    off += advance as i32;
                                    remaining -= chunk_bits;
                                }
                            }
                            // Skip the rest of the static offset handling
                        } else {
                            let src_vreg = ctx.reg_map.get(*src_reg);
                            let byte_off = ctx.byte_offset(addr, *bit_off);
                            let intra_byte = bit_off % 8;

                            if let Some(size) =
                                ctx.full_static_store_size(addr, *bit_off, *width_bits)
                            {
                                let src_vreg =
                                    ctx.mask_for_store_width(block, src_vreg, *width_bits);
                                block.push(MInst::Store {
                                    base: BaseReg::SimState,
                                    offset: byte_off,
                                    src: src_vreg,
                                    size,
                                });
                            } else if intra_byte == 0 && OpSize::from_bits(*width_bits).is_some() {
                                // Word-aligned, native size: direct store
                                block.push(MInst::Store {
                                    base: BaseReg::SimState,
                                    offset: byte_off,
                                    src: src_vreg,
                                    size: OpSize::from_bits(*width_bits).unwrap(),
                                });
                            } else {
                                let mut consumed = 0usize;
                                while consumed < *width_bits {
                                    let part_bit_off = *bit_off + consumed;
                                    let intra = part_bit_off % 8;
                                    let part_width = (*width_bits - consumed).min(64 - intra);
                                    let containing_byte_off =
                                        ctx.byte_offset(addr, 0) + (part_bit_off / 8) as i32;
                                    let load_size =
                                        ISelContext::op_size_for_width(part_width + intra);

                                    let part_src = if consumed == 0 {
                                        src_vreg
                                    } else {
                                        let shifted = ctx.alloc_vreg(SpillDesc::transient());
                                        block.push(MInst::ShrImm {
                                            dst: shifted,
                                            src: src_vreg,
                                            imm: consumed as u8,
                                        });
                                        shifted
                                    };

                                    let old_word = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::Load {
                                        dst: old_word,
                                        base: BaseReg::SimState,
                                        offset: containing_byte_off,
                                        size: load_size,
                                    });

                                    let new_word = ctx.alloc_vreg(SpillDesc::transient());
                                    ctx.emit_bfi(
                                        block,
                                        new_word,
                                        old_word,
                                        part_src,
                                        intra as u8,
                                        mask_for_width(part_width),
                                    );

                                    block.push(MInst::Store {
                                        base: BaseReg::SimState,
                                        offset: containing_byte_off,
                                        src: new_word,
                                        size: load_size,
                                    });

                                    consumed += part_width;
                                }
                            }
                        }
                    }
                    SIROffset::Dynamic(offset_reg) => {
                        // Dynamic offset store: RMW with register-indexed addressing.
                        let src_vreg = ctx.reg_map.get(*src_reg);
                        let offset_vreg = ctx.reg_map.get(*offset_reg);
                        let base_off = ctx.byte_offset(addr, 0);

                        let byte_off = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShrImm {
                            dst: byte_off,
                            src: offset_vreg,
                            imm: 3,
                        });
                        if *width_bits > 64 && low_zero_bits_reg(ctx, *offset_reg) >= 3 {
                            let chunks = ctx.get_wide_chunks(src_reg, block);
                            emit_aligned_dynamic_wide_store(
                                ctx,
                                block,
                                base_off,
                                byte_off,
                                *width_bits,
                                &chunks,
                            );
                        } else if *width_bits > 64 {
                            let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
                            ctx.emit_and_imm(block, bit_shift, offset_vreg, 7);
                            let chunks = ctx.get_wide_chunks(src_reg, block);
                            if let Some(changed) = emit_dynamic_wide_bitfield_store(
                                ctx,
                                block,
                                base_off,
                                byte_off,
                                bit_shift,
                                *width_bits,
                                &chunks,
                                !comb_capture_sites.is_empty(),
                            ) {
                                emit_enable_comb_capture_sites(
                                    ctx,
                                    block,
                                    changed,
                                    comb_capture_sites,
                                );
                            }
                        } else if low_zero_bits_reg(ctx, *offset_reg) >= 3
                            && let Some(store_size) = OpSize::from_bits(*width_bits)
                        {
                            let store_src = if *width_bits < 64 {
                                let masked = ctx.alloc_vreg(SpillDesc::transient());
                                ctx.emit_and_imm(
                                    block,
                                    masked,
                                    src_vreg,
                                    mask_for_width(*width_bits),
                                );
                                masked
                            } else {
                                src_vreg
                            };
                            let old_word = if comb_capture_sites.is_empty() {
                                None
                            } else {
                                let old = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::LoadIndexed {
                                    dst: old,
                                    base: BaseReg::SimState,
                                    offset: base_off,
                                    index: byte_off,
                                    size: store_size,
                                });
                                Some(old)
                            };
                            block.push(MInst::StoreIndexed {
                                base: BaseReg::SimState,
                                offset: base_off,
                                index: byte_off,
                                src: store_src,
                                size: store_size,
                            });
                            if let Some(old_word) = old_word {
                                let changed = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Cmp {
                                    dst: changed,
                                    lhs: old_word,
                                    rhs: store_src,
                                    kind: CmpKind::Ne,
                                });
                                emit_enable_comb_capture_sites(
                                    ctx,
                                    block,
                                    changed,
                                    comb_capture_sites,
                                );
                            }
                        } else {
                            let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
                            ctx.emit_and_imm(block, bit_shift, offset_vreg, 7);
                            if let Some(changed) = emit_dynamic_scalar_bitfield_store(
                                ctx,
                                block,
                                base_off,
                                byte_off,
                                bit_shift,
                                src_vreg,
                                *width_bits,
                                !comb_capture_sites.is_empty(),
                            ) {
                                emit_enable_comb_capture_sites(
                                    ctx,
                                    block,
                                    changed,
                                    comb_capture_sites,
                                );
                            }
                        }
                    }
                }

                // 4-state: store mask to memory
                if ctx.is_4state_var(addr) {
                    let mask_vreg = ctx.get_mask(*src_reg, block);
                    match offset {
                        SIROffset::Static(bit_off) => {
                            if *width_bits > 64 {
                                // Wide mask store: chunk-by-chunk
                                if let Some(mchunks) = ctx.wide_masks.get(src_reg).cloned() {
                                    let mut bit_pos = 0usize;
                                    let mut store_remaining = *width_bits;
                                    for (chunk_vreg, chunk_width) in &mchunks {
                                        if store_remaining == 0 {
                                            break;
                                        }
                                        let logical_chunk_width =
                                            (*chunk_width).min(store_remaining);
                                        let mut consumed = 0usize;
                                        while consumed < logical_chunk_width {
                                            let part_bit_off = *bit_off + bit_pos + consumed;
                                            let intra = part_bit_off % 8;
                                            let remaining = logical_chunk_width - consumed;
                                            let part_width = remaining.min(64 - intra);
                                            let part_byte_off =
                                                ctx.mask_byte_offset(addr, part_bit_off);
                                            let part_src = if consumed == 0 {
                                                *chunk_vreg
                                            } else {
                                                let shifted =
                                                    ctx.alloc_vreg(SpillDesc::transient());
                                                block.push(MInst::ShrImm {
                                                    dst: shifted,
                                                    src: *chunk_vreg,
                                                    imm: consumed as u8,
                                                });
                                                shifted
                                            };

                                            if intra == 0 && OpSize::from_bits(part_width).is_some()
                                            {
                                                block.push(MInst::Store {
                                                    base: BaseReg::SimState,
                                                    offset: part_byte_off,
                                                    src: part_src,
                                                    size: OpSize::from_bits(part_width).unwrap(),
                                                });
                                            } else {
                                                let containing_off = ctx.mask_byte_offset(addr, 0)
                                                    + (part_bit_off / 8) as i32;
                                                let load_size = ISelContext::op_size_for_width(
                                                    part_width + intra,
                                                );
                                                let old = ctx.alloc_vreg(SpillDesc::transient());
                                                block.push(MInst::Load {
                                                    dst: old,
                                                    base: BaseReg::SimState,
                                                    offset: containing_off,
                                                    size: load_size,
                                                });
                                                let new = ctx.alloc_vreg(SpillDesc::transient());
                                                ctx.emit_bfi(
                                                    block,
                                                    new,
                                                    old,
                                                    part_src,
                                                    intra as u8,
                                                    mask_for_width(part_width),
                                                );
                                                block.push(MInst::Store {
                                                    base: BaseReg::SimState,
                                                    offset: containing_off,
                                                    src: new,
                                                    size: load_size,
                                                });
                                            }
                                            consumed += part_width;
                                        }
                                        bit_pos += logical_chunk_width;
                                        store_remaining -= logical_chunk_width;
                                    }
                                }
                            } else {
                                let mask_off = ctx.mask_byte_offset(addr, *bit_off);
                                let intra_byte = bit_off % 8;
                                if let Some(size) =
                                    ctx.full_static_store_size(addr, *bit_off, *width_bits)
                                {
                                    let mask_vreg =
                                        ctx.mask_for_store_width(block, mask_vreg, *width_bits);
                                    block.push(MInst::Store {
                                        base: BaseReg::SimState,
                                        offset: mask_off,
                                        src: mask_vreg,
                                        size,
                                    });
                                } else if intra_byte == 0
                                    && OpSize::from_bits(*width_bits).is_some()
                                {
                                    block.push(MInst::Store {
                                        base: BaseReg::SimState,
                                        offset: mask_off,
                                        src: mask_vreg,
                                        size: OpSize::from_bits(*width_bits).unwrap(),
                                    });
                                } else {
                                    let mut consumed = 0usize;
                                    while consumed < *width_bits {
                                        let part_bit_off = *bit_off + consumed;
                                        let intra = part_bit_off % 8;
                                        let part_width = (*width_bits - consumed).min(64 - intra);
                                        let containing_off = ctx.mask_byte_offset(addr, 0)
                                            + (part_bit_off / 8) as i32;
                                        let load_size =
                                            ISelContext::op_size_for_width(part_width + intra);
                                        let part_src = if consumed == 0 {
                                            mask_vreg
                                        } else {
                                            let shifted = ctx.alloc_vreg(SpillDesc::transient());
                                            block.push(MInst::ShrImm {
                                                dst: shifted,
                                                src: mask_vreg,
                                                imm: consumed as u8,
                                            });
                                            shifted
                                        };
                                        let old = ctx.alloc_vreg(SpillDesc::transient());
                                        block.push(MInst::Load {
                                            dst: old,
                                            base: BaseReg::SimState,
                                            offset: containing_off,
                                            size: load_size,
                                        });
                                        let new_word = ctx.alloc_vreg(SpillDesc::transient());
                                        ctx.emit_bfi(
                                            block,
                                            new_word,
                                            old,
                                            part_src,
                                            intra as u8,
                                            mask_for_width(part_width),
                                        );
                                        block.push(MInst::Store {
                                            base: BaseReg::SimState,
                                            offset: containing_off,
                                            src: new_word,
                                            size: load_size,
                                        });
                                        consumed += part_width;
                                    }
                                }
                            }
                        }
                        SIROffset::Dynamic(offset_reg) => {
                            // Dynamic mask store: same RMW pattern as value store,
                            // but targeting the mask memory region.
                            let offset_vreg = ctx.reg_map.get(*offset_reg);
                            let mask_base_off = ctx.mask_byte_offset(addr, 0);

                            let m_byte_off = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShrImm {
                                dst: m_byte_off,
                                src: offset_vreg,
                                imm: 3,
                            });
                            if *width_bits > 64 && low_zero_bits_reg(ctx, *offset_reg) >= 3 {
                                let n_chunks = width_bits.div_ceil(64);
                                let mask_vregs =
                                    get_wide_mask_chunks(ctx, block, src_reg, n_chunks);
                                let mask_chunks = mask_vregs
                                    .into_iter()
                                    .enumerate()
                                    .map(|(index, chunk)| {
                                        (chunk, (*width_bits - index * 64).min(64))
                                    })
                                    .collect::<Vec<_>>();
                                emit_aligned_dynamic_wide_store(
                                    ctx,
                                    block,
                                    mask_base_off,
                                    m_byte_off,
                                    *width_bits,
                                    &mask_chunks,
                                );
                            } else if *width_bits > 64 {
                                let m_bit_shift = ctx.alloc_vreg(SpillDesc::transient());
                                ctx.emit_and_imm(block, m_bit_shift, offset_vreg, 7);
                                let n_chunks = width_bits.div_ceil(64);
                                let mask_chunks =
                                    get_wide_mask_chunks(ctx, block, src_reg, n_chunks)
                                        .into_iter()
                                        .enumerate()
                                        .map(|(index, chunk)| {
                                            (chunk, (*width_bits - index * 64).min(64))
                                        })
                                        .collect::<Vec<_>>();
                                if let Some(changed) = emit_dynamic_wide_bitfield_store(
                                    ctx,
                                    block,
                                    mask_base_off,
                                    m_byte_off,
                                    m_bit_shift,
                                    *width_bits,
                                    &mask_chunks,
                                    !comb_capture_sites.is_empty(),
                                ) {
                                    emit_enable_comb_capture_sites(
                                        ctx,
                                        block,
                                        changed,
                                        comb_capture_sites,
                                    );
                                }
                            } else if low_zero_bits_reg(ctx, *offset_reg) >= 3
                                && let Some(store_size) = OpSize::from_bits(*width_bits)
                            {
                                let store_src = if *width_bits < 64 {
                                    let masked = ctx.alloc_vreg(SpillDesc::transient());
                                    ctx.emit_and_imm(
                                        block,
                                        masked,
                                        mask_vreg,
                                        mask_for_width(*width_bits),
                                    );
                                    masked
                                } else {
                                    mask_vreg
                                };
                                block.push(MInst::StoreIndexed {
                                    base: BaseReg::SimState,
                                    offset: mask_base_off,
                                    index: m_byte_off,
                                    src: store_src,
                                    size: store_size,
                                });
                            } else {
                                let m_bit_shift = ctx.alloc_vreg(SpillDesc::transient());
                                ctx.emit_and_imm(block, m_bit_shift, offset_vreg, 7);
                                if let Some(changed) = emit_dynamic_scalar_bitfield_store(
                                    ctx,
                                    block,
                                    mask_base_off,
                                    m_byte_off,
                                    m_bit_shift,
                                    mask_vreg,
                                    *width_bits,
                                    !comb_capture_sites.is_empty(),
                                ) {
                                    emit_enable_comb_capture_sites(
                                        ctx,
                                        block,
                                        changed,
                                        comb_capture_sites,
                                    );
                                }
                            }
                        }
                    }
                }

                if let Some((old_comb_probe, byte_off, size)) = old_comb_probe {
                    let new_comb_probe = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Load {
                        dst: new_comb_probe,
                        base: BaseReg::SimState,
                        offset: byte_off,
                        size,
                    });
                    let changed = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: changed,
                        lhs: old_comb_probe,
                        rhs: new_comb_probe,
                        kind: CmpKind::Ne,
                    });
                    emit_enable_comb_capture_sites(ctx, block, changed, comb_capture_sites);
                }
                if !old_comb_wide_probe.is_empty() {
                    emit_enable_comb_capture_sites_if_byte_probes_changed(
                        ctx,
                        block,
                        old_comb_wide_probe,
                        comb_capture_sites,
                    );
                }
                if let Some((old_comb_mask_probe, byte_off, size)) = old_comb_mask_probe {
                    let new_comb_mask_probe = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Load {
                        dst: new_comb_mask_probe,
                        base: BaseReg::SimState,
                        offset: byte_off,
                        size,
                    });
                    let changed = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: changed,
                        lhs: old_comb_mask_probe,
                        rhs: new_comb_mask_probe,
                        kind: CmpKind::Ne,
                    });
                    emit_enable_comb_capture_sites(ctx, block, changed, comb_capture_sites);
                }
                if !old_comb_wide_mask_probe.is_empty() {
                    emit_enable_comb_capture_sites_if_byte_probes_changed(
                        ctx,
                        block,
                        old_comb_wide_mask_probe,
                        comb_capture_sites,
                    );
                }

                // Trigger detection: compare old vs new value, set triggered_bits
                if !triggers.is_empty() {
                    if let SIROffset::Static(bit_off) = offset {
                        // Load new value (just stored)
                        let byte_off = ctx.byte_offset(addr, *bit_off);
                        let new_val = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Load {
                            dst: new_val,
                            base: BaseReg::SimState,
                            offset: byte_off,
                            size: ISelContext::op_size_for_width(*width_bits),
                        });

                        for trigger in triggers {
                            let trigger_byte_idx = trigger.id / 8;
                            let trigger_bit_idx = trigger.id % 8;
                            let trigger_offset =
                                ctx.layout.triggered_bits_offset + trigger_byte_idx;

                            // Check new_val for trigger condition
                            let triggered = ctx.alloc_vreg(SpillDesc::transient());
                            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                            block.push(MInst::LoadImm {
                                dst: zero,
                                value: 0,
                            });

                            // For posedge/async_high: triggered if new_val != 0
                            // (old value comparison is handled by Simulation's step())
                            block.push(MInst::Cmp {
                                dst: triggered,
                                lhs: new_val,
                                rhs: zero,
                                kind: CmpKind::Ne,
                            });

                            // Load current triggered byte, OR in the bit, store back
                            let old_byte = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Load {
                                dst: old_byte,
                                base: BaseReg::SimState,
                                offset: trigger_offset as i32,
                                size: OpSize::S8,
                            });

                            let bit_mask =
                                ctx.alloc_vreg(SpillDesc::remat(1u64 << trigger_bit_idx));
                            block.push(MInst::LoadImm {
                                dst: bit_mask,
                                value: 1u64 << trigger_bit_idx,
                            });

                            // conditional: if triggered, OR in the bit
                            let selected_mask = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Select {
                                dst: selected_mask,
                                cond: triggered,
                                true_val: bit_mask,
                                false_val: zero,
                            });

                            let new_byte = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or {
                                dst: new_byte,
                                lhs: old_byte,
                                rhs: selected_mask,
                            });

                            block.push(MInst::Store {
                                base: BaseReg::SimState,
                                offset: trigger_offset as i32,
                                src: new_byte,
                                size: OpSize::S8,
                            });
                        }
                    }
                }
            } // else (width != 0)
        } // Store

        SIRInstruction::Commit(src_addr, dst_addr, offset, width_bits, _triggers) => {
            ctx.trigger_only_seen.clear();
            // Commit = load from src region, store to dst region (same offset/width)
            match offset {
                SIROffset::Static(bit_off) => {
                    if bit_off % 8 == 0 && width_bits % 8 == 0 && *width_bits >= 512 {
                        let byte_off = bit_off / 8;
                        block.push(MInst::MemCopy {
                            src_offset: ctx.byte_offset(src_addr, 0) + byte_off as i32,
                            dst_offset: ctx.byte_offset(dst_addr, 0) + byte_off as i32,
                            byte_len: width_bits / 8,
                        });
                    } else {
                        let mut copied = 0usize;
                        while copied < *width_bits {
                            let part_bit_off = *bit_off + copied;
                            let intra = part_bit_off % 8;
                            let part_width = (*width_bits - copied).min(64 - intra);
                            let load_size = ISelContext::op_size_for_width(part_width + intra);
                            let containing_src =
                                ctx.byte_offset(src_addr, 0) + (part_bit_off / 8) as i32;
                            let containing_dst =
                                ctx.byte_offset(dst_addr, 0) + (part_bit_off / 8) as i32;

                            let raw = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Load {
                                dst: raw,
                                base: BaseReg::SimState,
                                offset: containing_src,
                                size: load_size,
                            });

                            let shifted = if intra > 0 {
                                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShrImm {
                                    dst: shifted,
                                    src: raw,
                                    imm: intra as u8,
                                });
                                shifted
                            } else {
                                raw
                            };
                            let value = ctx.alloc_vreg(SpillDesc::transient());
                            ctx.emit_and_imm(block, value, shifted, mask_for_width(part_width));

                            let old = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Load {
                                dst: old,
                                base: BaseReg::SimState,
                                offset: containing_dst,
                                size: load_size,
                            });
                            let new = ctx.alloc_vreg(SpillDesc::transient());
                            ctx.emit_bfi(
                                block,
                                new,
                                old,
                                value,
                                intra as u8,
                                mask_for_width(part_width),
                            );
                            block.push(MInst::Store {
                                base: BaseReg::SimState,
                                offset: containing_dst,
                                src: new,
                                size: load_size,
                            });

                            copied += part_width;
                        }
                    }
                }
                SIROffset::Dynamic(offset_reg) => {
                    // Dynamic offset commit: copy from src to dst region.
                    // Both use the same dynamic offset.
                    let offset_vreg = ctx.reg_map.get(*offset_reg);
                    let src_base_off = ctx.byte_offset(src_addr, 0);
                    let dst_base_off = ctx.byte_offset(dst_addr, 0);

                    let byte_off = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: byte_off,
                        src: offset_vreg,
                        imm: 3,
                    });

                    // For simplicity, copy the containing bytes chunk-by-chunk.
                    // The physical width covers width_bits + up to 7 bit shift.
                    let phys_bytes = (*width_bits).div_ceil(8);
                    let mut copied = 0usize;
                    while copied < phys_bytes {
                        let remaining = phys_bytes - copied;
                        let chunk_size = if remaining >= 8 {
                            OpSize::S64
                        } else if remaining >= 4 {
                            OpSize::S32
                        } else if remaining >= 2 {
                            OpSize::S16
                        } else {
                            OpSize::S8
                        };
                        let tmp = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::LoadIndexed {
                            dst: tmp,
                            base: BaseReg::SimState,
                            offset: src_base_off + copied as i32,
                            index: byte_off,
                            size: chunk_size,
                        });
                        block.push(MInst::StoreIndexed {
                            base: BaseReg::SimState,
                            offset: dst_base_off + copied as i32,
                            index: byte_off,
                            src: tmp,
                            size: chunk_size,
                        });
                        copied += chunk_size.bytes() as usize;
                    }
                }
            }

            // 4-state: also commit mask
            if ctx.is_4state_var(src_addr) && ctx.is_4state_var(dst_addr) {
                match offset {
                    SIROffset::Static(bit_off) => {
                        if bit_off % 8 == 0 && width_bits % 8 == 0 && *width_bits >= 512 {
                            let byte_off = bit_off / 8;
                            block.push(MInst::MemCopy {
                                src_offset: ctx.mask_byte_offset(src_addr, 0) + byte_off as i32,
                                dst_offset: ctx.mask_byte_offset(dst_addr, 0) + byte_off as i32,
                                byte_len: width_bits / 8,
                            });
                        } else {
                            let mut copied = 0usize;
                            while copied < *width_bits {
                                let part_bit_off = *bit_off + copied;
                                let intra = part_bit_off % 8;
                                let part_width = (*width_bits - copied).min(64 - intra);
                                let load_size = ISelContext::op_size_for_width(part_width + intra);
                                let containing_src =
                                    ctx.mask_byte_offset(src_addr, 0) + (part_bit_off / 8) as i32;
                                let containing_dst =
                                    ctx.mask_byte_offset(dst_addr, 0) + (part_bit_off / 8) as i32;

                                let raw = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Load {
                                    dst: raw,
                                    base: BaseReg::SimState,
                                    offset: containing_src,
                                    size: load_size,
                                });
                                let shifted = if intra > 0 {
                                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::ShrImm {
                                        dst: shifted,
                                        src: raw,
                                        imm: intra as u8,
                                    });
                                    shifted
                                } else {
                                    raw
                                };
                                let value = ctx.alloc_vreg(SpillDesc::transient());
                                ctx.emit_and_imm(block, value, shifted, mask_for_width(part_width));

                                let old = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Load {
                                    dst: old,
                                    base: BaseReg::SimState,
                                    offset: containing_dst,
                                    size: load_size,
                                });
                                let new = ctx.alloc_vreg(SpillDesc::transient());
                                ctx.emit_bfi(
                                    block,
                                    new,
                                    old,
                                    value,
                                    intra as u8,
                                    mask_for_width(part_width),
                                );
                                block.push(MInst::Store {
                                    base: BaseReg::SimState,
                                    offset: containing_dst,
                                    src: new,
                                    size: load_size,
                                });

                                copied += part_width;
                            }
                        }
                    }
                    SIROffset::Dynamic(offset_reg) => {
                        let offset_vreg = ctx.reg_map.get(*offset_reg);
                        let src_mask_base = ctx.mask_byte_offset(src_addr, 0);
                        let dst_mask_base = ctx.mask_byte_offset(dst_addr, 0);
                        let byte_off = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShrImm {
                            dst: byte_off,
                            src: offset_vreg,
                            imm: 3,
                        });
                        let phys_bytes = (*width_bits).div_ceil(8);
                        let mut copied = 0usize;
                        while copied < phys_bytes {
                            let remaining = phys_bytes - copied;
                            let cs = if remaining >= 8 {
                                OpSize::S64
                            } else if remaining >= 4 {
                                OpSize::S32
                            } else if remaining >= 2 {
                                OpSize::S16
                            } else {
                                OpSize::S8
                            };
                            let tmp = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::LoadIndexed {
                                dst: tmp,
                                base: BaseReg::SimState,
                                offset: src_mask_base + copied as i32,
                                index: byte_off,
                                size: cs,
                            });
                            block.push(MInst::StoreIndexed {
                                base: BaseReg::SimState,
                                offset: dst_mask_base + copied as i32,
                                index: byte_off,
                                src: tmp,
                                size: cs,
                            });
                            copied += cs.bytes() as usize;
                        }
                    }
                }
            }
        }

        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            let d_width = ctx.sir_width(dst);
            let lhs_width = ctx.sir_width(lhs);
            let rhs_width = ctx.sir_width(rhs);

            // Wide (>64-bit) binary operations: dispatch to multi-word handler.
            // For comparisons/logic, the result may be narrow (1 bit) but the
            // operands can be wide — dispatch based on operand width too.
            if d_width > 64 || lhs_width > 64 || rhs_width > 64 {
                lower_wide_binary(ctx, block, *dst, *lhs, op, *rhs);
                if ctx.four_state {
                    lower_wide_binary_mask(ctx, block, *dst, *lhs, op, *rhs, d_width);
                    normalize_wide_value(ctx, block, *dst);
                }
                return;
            }
            // Also check if operands have wide chunks (may have been loaded wide
            // even if sir_width reports ≤64 due to optimizer width changes).
            if ctx.wide_regs.contains_key(lhs) || ctx.wide_regs.contains_key(rhs) {
                lower_wide_binary(ctx, block, *dst, *lhs, op, *rhs);
                if ctx.four_state {
                    lower_wide_binary_mask(ctx, block, *dst, *lhs, op, *rhs, d_width);
                    normalize_wide_value(ctx, block, *dst);
                }
                return;
            }

            // Constant folding: if both operands are known constants, compute result
            let lhs_const = ctx.consts.get(lhs).copied();
            let rhs_const = ctx.consts.get(rhs).copied();
            if let (Some(lc), Some(rc)) = (lhs_const, rhs_const) {
                let result = match op {
                    BinaryOp::Add => Some(lc.wrapping_add(rc)),
                    BinaryOp::Sub => Some(lc.wrapping_sub(rc)),
                    BinaryOp::Mul => Some(lc.wrapping_mul(rc)),
                    BinaryOp::And => Some(lc & rc),
                    BinaryOp::Or => Some(lc | rc),
                    BinaryOp::Xor => Some(lc ^ rc),
                    BinaryOp::Shl => Some(if rc >= 64 { 0 } else { lc << rc }),
                    BinaryOp::Shr => Some(if rc >= 64 { 0 } else { lc >> rc }),
                    _ => None,
                };
                if let Some(val) = result {
                    let mask = mask_for_width(d_width);
                    let val = val & mask;
                    let dst_vreg = ctx.reg_map.get(*dst);
                    ctx.spill_descs[dst_vreg.0 as usize] = SpillDesc::remat(val);
                    block.push(MInst::LoadImm {
                        dst: dst_vreg,
                        value: val,
                    });
                    ctx.consts.insert(*dst, val);
                    set_low_zero_bits(ctx, *dst, low_zero_bits_const(val));
                    // 4-state: constants always have mask=0
                    if ctx.four_state {
                        let z = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm { dst: z, value: 0 });
                        ctx.set_mask(*dst, z);
                    }
                    return;
                }
            }

            let dst_vreg = ctx.reg_map.get(*dst);
            let lhs_vreg = ctx.reg_map.get(*lhs);
            let rhs_vreg = ctx.reg_map.get(*rhs);

            match op {
                BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul => {
                    // 64-bit arithmetic may produce upper bits; mask to output width.
                    let raw = if d_width < 64 {
                        ctx.alloc_vreg(SpillDesc::transient())
                    } else {
                        dst_vreg
                    };
                    match op {
                        BinaryOp::Add => block.push(MInst::Add {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Sub => block.push(MInst::Sub {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Mul => block.push(MInst::Mul {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        _ => unreachable!(),
                    }
                    if d_width < 64 {
                        ctx.emit_and_imm(block, dst_vreg, raw, mask_for_width(d_width));
                    }
                    let lhs_lz = low_zero_bits_reg(ctx, *lhs);
                    let rhs_lz = low_zero_bits_reg(ctx, *rhs);
                    let lz = match op {
                        BinaryOp::Add | BinaryOp::Sub => lhs_lz.min(rhs_lz),
                        BinaryOp::Mul => {
                            let lhs_const = ctx.consts.get(lhs).copied();
                            let rhs_const = ctx.consts.get(rhs).copied();
                            match (lhs_const, rhs_const) {
                                (_, Some(rc)) => lhs_lz.saturating_add(low_zero_bits_const(rc)),
                                (Some(lc), _) => rhs_lz.saturating_add(low_zero_bits_const(lc)),
                                _ => lhs_lz.saturating_add(rhs_lz),
                            }
                        }
                        _ => unreachable!(),
                    };
                    set_low_zero_bits(ctx, *dst, lz);
                }
                BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
                    // For bitwise ops, result width = max(lhs_bits, rhs_bits).
                    // If both inputs fit within d_width, AND mask is redundant.
                    let lhs_bits = ctx.known_bits.get(&lhs_vreg).copied().unwrap_or(64);
                    let rhs_bits = ctx.known_bits.get(&rhs_vreg).copied().unwrap_or(64);
                    let result_bits = lhs_bits.max(rhs_bits);
                    let needs_mask = d_width < 64 && result_bits > d_width;

                    let raw = if needs_mask {
                        ctx.alloc_vreg(SpillDesc::transient())
                    } else {
                        dst_vreg
                    };
                    match op {
                        BinaryOp::And => block.push(MInst::And {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Or => block.push(MInst::Or {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        BinaryOp::Xor => block.push(MInst::Xor {
                            dst: raw,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        }),
                        _ => unreachable!(),
                    }
                    if needs_mask {
                        ctx.emit_and_imm(block, dst_vreg, raw, mask_for_width(d_width));
                    }
                    let lhs_lz = low_zero_bits_reg(ctx, *lhs);
                    let rhs_lz = low_zero_bits_reg(ctx, *rhs);
                    let lz = match op {
                        BinaryOp::And => lhs_lz.max(rhs_lz),
                        BinaryOp::Or | BinaryOp::Xor => lhs_lz.min(rhs_lz),
                        _ => unreachable!(),
                    };
                    set_low_zero_bits(ctx, *dst, lz);
                }
                BinaryOp::Shr => {
                    // Check for wide-to-narrow extraction: lhs is >64 bits, dst is ≤64 bits
                    let lhs_width = ctx.sir_width(lhs);
                    if lhs_width > 64 {
                        lower_wide_extract(ctx, block, *dst, *lhs, *rhs);
                        return;
                    }

                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    if let Some(&shift_amt) = ctx.consts.get(rhs) {
                        if shift_amt < 64 {
                            block.push(MInst::ShrImm {
                                dst: shifted,
                                src: lhs_vreg,
                                imm: shift_amt as u8,
                            });
                        } else {
                            block.push(MInst::LoadImm {
                                dst: shifted,
                                value: 0,
                            });
                        }
                        // Track known bits: shr reduces width
                        let lhs_bits = ctx.known_bits.get(&lhs_vreg).copied().unwrap_or(64);
                        let shifted_bits = lhs_bits
                            .saturating_sub(usize::try_from(shift_amt).unwrap_or(usize::MAX));
                        ctx.known_bits.insert(shifted, shifted_bits);
                    } else {
                        let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
                        ctx.emit_mov(block, rhs_copy, rhs_vreg);
                        block.push(MInst::Shr {
                            dst: shifted,
                            lhs: lhs_vreg,
                            rhs: rhs_copy,
                        });
                    }
                    // Mask to destination width
                    if d_width < 64 {
                        let mask = mask_for_width(d_width);
                        ctx.emit_and_imm(block, dst_vreg, shifted, mask);
                    } else {
                        ctx.emit_mov(block, dst_vreg, shifted);
                    }
                    let lz = if let Some(&shift_amt) = ctx.consts.get(rhs) {
                        low_zero_bits_reg(ctx, *lhs)
                            .saturating_sub(u32::try_from(shift_amt).unwrap_or(u32::MAX))
                    } else {
                        0
                    };
                    set_low_zero_bits(ctx, *dst, lz);
                }
                BinaryOp::Shl => {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    if let Some(&shift_amt) = ctx.consts.get(rhs) {
                        if shift_amt < 64 {
                            block.push(MInst::ShlImm {
                                dst: shifted,
                                src: lhs_vreg,
                                imm: shift_amt as u8,
                            });
                        } else {
                            block.push(MInst::LoadImm {
                                dst: shifted,
                                value: 0,
                            });
                        }
                    } else {
                        let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
                        ctx.emit_mov(block, rhs_copy, rhs_vreg);
                        block.push(MInst::Shl {
                            dst: shifted,
                            lhs: lhs_vreg,
                            rhs: rhs_copy,
                        });
                    }
                    if d_width < 64 {
                        let mask = mask_for_width(d_width);
                        ctx.emit_and_imm(block, dst_vreg, shifted, mask);
                    } else {
                        ctx.emit_mov(block, dst_vreg, shifted);
                    }
                    let lz = if let Some(&shift_amt) = ctx.consts.get(rhs) {
                        low_zero_bits_reg(ctx, *lhs)
                            .saturating_add(u32::try_from(shift_amt).unwrap_or(u32::MAX))
                    } else {
                        0
                    };
                    set_low_zero_bits(ctx, *dst, lz);
                }
                BinaryOp::Sar => {
                    // Arithmetic shift right: sign-extend lhs to 64 bits, shift, mask result.
                    let width = ctx.sir_width(lhs);
                    if width < 64 {
                        let sext_shift = (64 - width) as u8;
                        let shifted_up = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShlImm {
                            dst: shifted_up,
                            src: lhs_vreg,
                            imm: sext_shift,
                        });
                        let sign_extended = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::SarImm {
                            dst: sign_extended,
                            src: shifted_up,
                            imm: sext_shift,
                        });
                        // Now do the actual shift
                        let sar_result = ctx.alloc_vreg(SpillDesc::transient());
                        if let Some(&shift_amt) = ctx.consts.get(rhs) {
                            block.push(MInst::SarImm {
                                dst: sar_result,
                                src: sign_extended,
                                imm: shift_amt.min(63) as u8,
                            });
                        } else {
                            let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
                            ctx.emit_mov(block, rhs_copy, rhs_vreg);
                            block.push(MInst::Sar {
                                dst: sar_result,
                                lhs: sign_extended,
                                rhs: rhs_copy,
                            });
                        }
                        // Mask to output width
                        let mask = mask_for_width(width);
                        ctx.emit_and_imm(block, dst_vreg, sar_result, mask);
                    } else {
                        if let Some(&shift_amt) = ctx.consts.get(rhs) {
                            block.push(MInst::SarImm {
                                dst: dst_vreg,
                                src: lhs_vreg,
                                imm: shift_amt.min(63) as u8,
                            });
                        } else {
                            let rhs_copy = ctx.alloc_vreg(SpillDesc::transient());
                            ctx.emit_mov(block, rhs_copy, rhs_vreg);
                            block.push(MInst::Sar {
                                dst: dst_vreg,
                                lhs: lhs_vreg,
                                rhs: rhs_copy,
                            });
                        }
                    }
                }
                BinaryOp::Eq => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::Eq,
                }),
                BinaryOp::Ne => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::Ne,
                }),
                BinaryOp::LtU => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::LtU,
                }),
                BinaryOp::LtS => {
                    let (sl, sr) = sign_extend_pair(ctx, block, lhs, rhs, lhs_vreg, rhs_vreg);
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: sl,
                        rhs: sr,
                        kind: CmpKind::LtS,
                    });
                }
                BinaryOp::LeU => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::LeU,
                }),
                BinaryOp::LeS => {
                    let (sl, sr) = sign_extend_pair(ctx, block, lhs, rhs, lhs_vreg, rhs_vreg);
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: sl,
                        rhs: sr,
                        kind: CmpKind::LeS,
                    });
                }
                BinaryOp::GtU => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::GtU,
                }),
                BinaryOp::GtS => {
                    let (sl, sr) = sign_extend_pair(ctx, block, lhs, rhs, lhs_vreg, rhs_vreg);
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: sl,
                        rhs: sr,
                        kind: CmpKind::GtS,
                    });
                }
                BinaryOp::GeU => block.push(MInst::Cmp {
                    dst: dst_vreg,
                    lhs: lhs_vreg,
                    rhs: rhs_vreg,
                    kind: CmpKind::GeU,
                }),
                BinaryOp::GeS => {
                    let (sl, sr) = sign_extend_pair(ctx, block, lhs, rhs, lhs_vreg, rhs_vreg);
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: sl,
                        rhs: sr,
                        kind: CmpKind::GeS,
                    });
                }
                BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS => {
                    let signed = matches!(op, BinaryOp::DivS | BinaryOp::RemS);
                    let lhs_width = ctx.sir_width(lhs);
                    let rhs_width = ctx.sir_width(rhs);
                    let division_lhs = if signed {
                        sign_extend_scalar(ctx, block, lhs_vreg, lhs_width)
                    } else {
                        lhs_vreg
                    };
                    let division_rhs = if signed {
                        sign_extend_scalar(ctx, block, rhs_vreg, rhs_width)
                    } else {
                        rhs_vreg
                    };

                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    let one = ctx.alloc_vreg(SpillDesc::remat(1));
                    block.push(MInst::LoadImm { dst: one, value: 1 });
                    let is_zero = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: is_zero,
                        lhs: division_rhs,
                        rhs: zero,
                        kind: CmpKind::Eq,
                    });

                    let unsafe_divisor = if signed {
                        let min = ctx.alloc_vreg(SpillDesc::remat(1u64 << 63));
                        block.push(MInst::LoadImm {
                            dst: min,
                            value: 1u64 << 63,
                        });
                        let neg_one = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
                        block.push(MInst::LoadImm {
                            dst: neg_one,
                            value: u64::MAX,
                        });
                        let is_min = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: is_min,
                            lhs: division_lhs,
                            rhs: min,
                            kind: CmpKind::Eq,
                        });
                        let is_neg_one = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: is_neg_one,
                            lhs: division_rhs,
                            rhs: neg_one,
                            kind: CmpKind::Eq,
                        });
                        let is_overflow = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: is_overflow,
                            lhs: is_min,
                            rhs: is_neg_one,
                        });
                        let unsafe_divisor = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: unsafe_divisor,
                            lhs: is_zero,
                            rhs: is_overflow,
                        });
                        unsafe_divisor
                    } else {
                        is_zero
                    };
                    let safe_rhs = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: safe_rhs,
                        cond: unsafe_divisor,
                        true_val: one,
                        false_val: division_rhs,
                    });
                    let division_result = ctx.alloc_vreg(SpillDesc::transient());
                    match op {
                        BinaryOp::DivU => block.push(MInst::UDiv {
                            dst: division_result,
                            lhs: division_lhs,
                            rhs: safe_rhs,
                        }),
                        BinaryOp::RemU => block.push(MInst::URem {
                            dst: division_result,
                            lhs: division_lhs,
                            rhs: safe_rhs,
                        }),
                        BinaryOp::DivS => block.push(MInst::SDiv {
                            dst: division_result,
                            lhs: division_lhs,
                            rhs: safe_rhs,
                        }),
                        BinaryOp::RemS => block.push(MInst::SRem {
                            dst: division_result,
                            lhs: division_lhs,
                            rhs: safe_rhs,
                        }),
                        _ => unreachable!(),
                    }
                    let defined_result = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: defined_result,
                        cond: is_zero,
                        true_val: zero,
                        false_val: division_result,
                    });
                    if d_width < 64 {
                        ctx.emit_and_imm(block, dst_vreg, defined_result, mask_for_width(d_width));
                    } else {
                        ctx.emit_mov(block, dst_vreg, defined_result);
                    }
                }
                BinaryOp::LogicAnd => {
                    // dst = (lhs != 0) && (rhs != 0) ? 1 : 0
                    let l_bool = lower_bool_value(ctx, block, lhs_vreg);
                    let r_bool = lower_bool_value(ctx, block, rhs_vreg);
                    block.push(MInst::And {
                        dst: dst_vreg,
                        lhs: l_bool,
                        rhs: r_bool,
                    });
                }
                BinaryOp::LogicOr => {
                    let l_bool = lower_bool_value(ctx, block, lhs_vreg);
                    let r_bool = lower_bool_value(ctx, block, rhs_vreg);
                    block.push(MInst::Or {
                        dst: dst_vreg,
                        lhs: l_bool,
                        rhs: r_bool,
                    });
                }
                BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
                    if ctx.four_state {
                        // IEEE 1800 ==?/!=?: RHS X/Z bits are wildcards (don't care)
                        let l_m = ctx.get_mask(*lhs, block);
                        let r_m = ctx.get_mask(*rhs, block);

                        // compare_mask = ~r_m (non-wildcard positions)
                        let compare_mask = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: compare_mask,
                            src: r_m,
                        });

                        // Compare only at non-wildcard positions
                        let l_eff = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: l_eff,
                            lhs: lhs_vreg,
                            rhs: compare_mask,
                        });
                        let r_eff = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: r_eff,
                            lhs: rhs_vreg,
                            rhs: compare_mask,
                        });

                        let kind = if matches!(op, BinaryOp::EqWildcard) {
                            CmpKind::Eq
                        } else {
                            CmpKind::Ne
                        };
                        block.push(MInst::Cmp {
                            dst: dst_vreg,
                            lhs: l_eff,
                            rhs: r_eff,
                            kind,
                        });

                        // Mask: check for LHS X at non-wildcard positions
                        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm {
                            dst: zero,
                            value: 0,
                        });
                        let x_at_compared = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: x_at_compared,
                            lhs: l_m,
                            rhs: compare_mask,
                        });
                        let has_x = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: has_x,
                            lhs: x_at_compared,
                            rhs: zero,
                            kind: CmpKind::Ne,
                        });

                        // If definite mismatch at compared positions → mask=0
                        let l_xor_r = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Xor {
                            dst: l_xor_r,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                        });
                        let l_definite = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: l_definite,
                            src: l_m,
                        });
                        let definite_compare = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: definite_compare,
                            lhs: compare_mask,
                            rhs: l_definite,
                        });
                        let mismatch = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: mismatch,
                            lhs: l_xor_r,
                            rhs: definite_compare,
                        });
                        let has_mismatch = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: has_mismatch,
                            lhs: mismatch,
                            rhs: zero,
                            kind: CmpKind::Ne,
                        });

                        // mask = has_mismatch ? 0 : (has_x ? 1 : 0)
                        let res_m = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Select {
                            dst: res_m,
                            cond: has_mismatch,
                            true_val: zero,
                            false_val: has_x,
                        });
                        ctx.set_mask(*dst, res_m);
                        // Skip the general 4-state mask computation below
                    } else {
                        // 2-state: wildcards are same as Eq/Ne
                        let kind = if matches!(op, BinaryOp::EqWildcard) {
                            CmpKind::Eq
                        } else {
                            CmpKind::Ne
                        };
                        block.push(MInst::Cmp {
                            dst: dst_vreg,
                            lhs: lhs_vreg,
                            rhs: rhs_vreg,
                            kind,
                        });
                    }
                }
            }

            if matches!(
                op,
                BinaryOp::Eq
                    | BinaryOp::Ne
                    | BinaryOp::LtU
                    | BinaryOp::LtS
                    | BinaryOp::LeU
                    | BinaryOp::LeS
                    | BinaryOp::GtU
                    | BinaryOp::GtS
                    | BinaryOp::GeU
                    | BinaryOp::GeS
                    | BinaryOp::LogicAnd
                    | BinaryOp::LogicOr
                    | BinaryOp::EqWildcard
                    | BinaryOp::NeWildcard
            ) {
                ctx.known_bits.insert(dst_vreg, 1);
            }

            // 4-state: compute result mask (skip for wildcards which handle it inline)
            if ctx.four_state && !matches!(op, BinaryOp::EqWildcard | BinaryOp::NeWildcard) {
                let l_m = ctx.get_mask(*lhs, block);
                let r_m = ctx.get_mask(*rhs, block);
                let res_m =
                    lower_binary_mask(ctx, block, op, lhs_vreg, rhs_vreg, l_m, r_m, d_width);
                ctx.set_mask(*dst, res_m);

                // Normalize: X positions must have v=1 (X encoding = v:1, m:1)
                let old_v = ctx.reg_map.get(*dst);
                let normalized = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: normalized,
                    lhs: old_v,
                    rhs: res_m,
                });
                ctx.reg_map.set(*dst, normalized);
            }
        }

        SIRInstruction::Unary(dst, op, src) => {
            let d_width = ctx.sir_width(dst);
            let src_width = ctx.sir_width(src);
            if d_width > 64 || src_width > 64 || ctx.wide_regs.contains_key(src) {
                lower_wide_unary(ctx, block, *dst, op, *src);
                if ctx.four_state {
                    lower_wide_unary_mask(ctx, block, *dst, op, *src, d_width, src_width);
                    if matches!(op, UnaryOp::ToTwoState) {
                        lower_wide_to_two_state(ctx, block, *dst, *src, d_width, src_width);
                    } else if !matches!(op, UnaryOp::Ident) {
                        normalize_wide_value(ctx, block, *dst);
                    }
                }
                return;
            }
            let dst_vreg = ctx.reg_map.get(*dst);
            let src_vreg = ctx.reg_map.get(*src);

            match op {
                UnaryOp::Ident | UnaryOp::ToTwoState => {
                    ctx.emit_mov(block, dst_vreg, src_vreg);
                }
                UnaryOp::Minus => {
                    if d_width < 64 {
                        let negated = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Neg {
                            dst: negated,
                            src: src_vreg,
                        });
                        ctx.emit_and_imm(block, dst_vreg, negated, mask_for_width(d_width));
                    } else {
                        block.push(MInst::Neg {
                            dst: dst_vreg,
                            src: src_vreg,
                        });
                    }
                }
                UnaryOp::BitNot => {
                    let width = ctx.sir_width(src);
                    if width < 64 {
                        let tmp = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: tmp,
                            src: src_vreg,
                        });
                        ctx.emit_and_imm(block, dst_vreg, tmp, mask_for_width(width));
                    } else {
                        block.push(MInst::BitNot {
                            dst: dst_vreg,
                            src: src_vreg,
                        });
                    }
                }
                UnaryOp::LogicNot => {
                    // dst = (src == 0) ? 1 : 0
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: src_vreg,
                        rhs: zero,
                        kind: CmpKind::Eq,
                    });
                }
                UnaryOp::And => {
                    // Reduction AND: dst = (src == all_ones_mask) ? 1 : 0
                    let width = ctx.sir_width(src);
                    let mask = if width >= 64 {
                        u64::MAX
                    } else {
                        mask_for_width(width)
                    };
                    let mask_vreg = ctx.alloc_vreg(SpillDesc::remat(mask));
                    block.push(MInst::LoadImm {
                        dst: mask_vreg,
                        value: mask,
                    });
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: src_vreg,
                        rhs: mask_vreg,
                        kind: CmpKind::Eq,
                    });
                }
                UnaryOp::Or => {
                    // Reduction OR: dst = (src != 0) ? 1 : 0
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    block.push(MInst::Cmp {
                        dst: dst_vreg,
                        lhs: src_vreg,
                        rhs: zero,
                        kind: CmpKind::Ne,
                    });
                }
                UnaryOp::Xor => {
                    // Reduction XOR: dst = popcount(src) & 1
                    // Uses x86 POPCNT (SSE4.2) for single-instruction parity.
                    let pc = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Popcnt {
                        dst: pc,
                        src: src_vreg,
                    });
                    ctx.emit_and_imm(block, dst_vreg, pc, 1);
                    ctx.known_bits.insert(dst_vreg, 1);
                }
                UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
                    lower_narrow_bit_count(ctx, block, dst_vreg, op, src_vreg, src_width);
                    ctx.known_bits.insert(dst_vreg, d_width);
                }
            }

            // 4-state: compute result mask for unary ops
            if ctx.four_state {
                let s_m = ctx.get_mask(*src, block);
                if matches!(op, UnaryOp::ToTwoState) {
                    let defined = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::BitNot {
                        dst: defined,
                        src: s_m,
                    });
                    let cleared = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::And {
                        dst: cleared,
                        lhs: ctx.reg_map.get(*dst),
                        rhs: defined,
                    });
                    ctx.reg_map.set(*dst, cleared);
                    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    ctx.set_mask(*dst, zero);
                    return;
                }
                let res_m = lower_unary_mask(ctx, block, op, src_vreg, s_m, d_width, src_width);
                ctx.set_mask(*dst, res_m);

                if !matches!(op, UnaryOp::Ident) {
                    // Identity/casts preserve X versus Z. Other unary
                    // operations normalize unknown bits to X.
                    let old_v = ctx.reg_map.get(*dst);
                    let normalized = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or {
                        dst: normalized,
                        lhs: old_v,
                        rhs: res_m,
                    });
                    ctx.reg_map.set(*dst, normalized);
                }
            }
        }

        SIRInstruction::Concat(dst, args) => {
            if try_lower_concat_of_muxes(ctx, block, *dst, args, sir_block, sir_defs) {
                return;
            }

            // Concat: build a wide value from chunks.
            // For ≤64-bit result, shift and OR the pieces together.
            let dst_vreg = ctx.reg_map.get(*dst);
            let dst_width = ctx.sir_width(dst);

            if dst_width <= 64 {
                // args are [MSB, ..., LSB]
                // Build from LSB to MSB
                let mut accumulated: Option<VReg> = None;
                let mut shift_pos = 0usize;

                for arg in args.iter().rev() {
                    let arg_vreg = ctx.reg_map.get(*arg);
                    let arg_width = ctx.sir_width(arg);

                    match accumulated {
                        None => {
                            // First (LSB) element
                            accumulated = Some(arg_vreg);
                        }
                        Some(acc) => {
                            // Shift this arg and OR with accumulator
                            let shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm {
                                dst: shifted,
                                src: arg_vreg,
                                imm: shift_pos as u8,
                            });
                            let merged = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or {
                                dst: merged,
                                lhs: acc,
                                rhs: shifted,
                            });
                            accumulated = Some(merged);
                        }
                    }
                    shift_pos += arg_width;
                }

                if let Some(result) = accumulated {
                    if result != dst_vreg {
                        ctx.emit_mov(block, dst_vreg, result);
                    }
                }

                // 4-state: concat masks the same way
                if ctx.four_state {
                    let mut m_acc: Option<VReg> = None;
                    let mut m_shift = 0usize;
                    for arg in args.iter().rev() {
                        let m = ctx.get_mask(*arg, block);
                        let aw = ctx.sir_width(arg);
                        match m_acc {
                            None => {
                                m_acc = Some(m);
                            }
                            Some(a) => {
                                let sh = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShlImm {
                                    dst: sh,
                                    src: m,
                                    imm: m_shift as u8,
                                });
                                let mg = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Or {
                                    dst: mg,
                                    lhs: a,
                                    rhs: sh,
                                });
                                m_acc = Some(mg);
                            }
                        }
                        m_shift += aw;
                    }
                    if let Some(m_res) = m_acc {
                        ctx.set_mask(*dst, m_res);
                    }
                }
            } else {
                // Wide concat (>64 bits): record chunk vregs for use by Store.
                // args are [MSB, ..., LSB]. Collect bits in LSB-first order,
                // then repack into uniform 64-bit chunks so Slice can use
                // bit_offset / 64 for indexing.
                let total_width = args.iter().map(|a| ctx.sir_width(a)).sum::<usize>();
                let n_dst_chunks = ISelContext::num_chunks(total_width);

                // Collect a flat bit stream: list of (vreg, width) in LSB-first order
                let mut flat_bits: Vec<(VReg, usize)> = Vec::new();
                for arg in args.iter().rev() {
                    let arg_width = ctx.sir_width(arg);
                    if arg_width > 64 {
                        let arg_chunks = ctx.get_wide_chunks(arg, block);
                        for ch in arg_chunks {
                            flat_bits.push(ch);
                        }
                    } else {
                        let arg_vreg = ctx.reg_map.get(*arg);
                        flat_bits.push((arg_vreg, arg_width));
                    }
                }

                // Repack into 64-bit uniform chunks via shift+or
                let mut dst_chunks: Vec<(VReg, usize)> = Vec::new();
                let mut _bit_pos = 0usize; // current position in the flat stream
                let mut flat_idx = 0usize;
                let mut flat_consumed = 0usize; // bits consumed from flat_bits[flat_idx]

                for chunk_i in 0..n_dst_chunks {
                    let chunk_width = if chunk_i == n_dst_chunks - 1 {
                        let rem = total_width % 64;
                        if rem == 0 { 64 } else { rem }
                    } else {
                        64
                    };

                    let mut acc = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: acc, value: 0 });
                    let mut acc_pos = 0usize;

                    while acc_pos < chunk_width && flat_idx < flat_bits.len() {
                        let (fv, fw) = flat_bits[flat_idx];
                        let remaining_in_flat = fw - flat_consumed;
                        let need = chunk_width - acc_pos;
                        let take = remaining_in_flat.min(need);

                        // Extract `take` bits from fv starting at flat_consumed
                        let mut piece = fv;
                        if flat_consumed > 0 {
                            let shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShrImm {
                                dst: shifted,
                                src: piece,
                                imm: flat_consumed as u8,
                            });
                            piece = shifted;
                        }
                        if take < 64 {
                            let masked = ctx.alloc_vreg(SpillDesc::transient());
                            ctx.emit_and_imm(block, masked, piece, mask_for_width(take));
                            piece = masked;
                        }

                        // Place into acc at acc_pos
                        if acc_pos > 0 {
                            let shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm {
                                dst: shifted,
                                src: piece,
                                imm: acc_pos as u8,
                            });
                            let merged = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or {
                                dst: merged,
                                lhs: acc,
                                rhs: shifted,
                            });
                            acc = merged;
                        } else {
                            let merged = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or {
                                dst: merged,
                                lhs: acc,
                                rhs: piece,
                            });
                            acc = merged;
                        }

                        acc_pos += take;
                        flat_consumed += take;
                        if flat_consumed >= fw {
                            flat_idx += 1;
                            flat_consumed = 0;
                        }
                    }

                    dst_chunks.push((acc, chunk_width));
                    _bit_pos += chunk_width;
                }

                ctx.set_wide_chunks(*dst, dst_chunks);

                // 4-state: repack mask chunks the same way
                if ctx.four_state {
                    let mut mask_flat: Vec<(VReg, usize)> = Vec::new();
                    for arg in args.iter().rev() {
                        let arg_width = ctx.sir_width(arg);
                        if arg_width > 64 {
                            let mc = get_wide_mask_chunks(
                                ctx,
                                block,
                                arg,
                                ISelContext::num_chunks(arg_width),
                            );
                            for (i, mv) in mc.into_iter().enumerate() {
                                let cw = if i == ISelContext::num_chunks(arg_width) - 1 {
                                    let r = arg_width % 64;
                                    if r == 0 { 64 } else { r }
                                } else {
                                    64
                                };
                                mask_flat.push((mv, cw));
                            }
                        } else {
                            let m = ctx.get_mask(*arg, block);
                            mask_flat.push((m, arg_width));
                        }
                    }

                    // Repack masks into uniform 64-bit chunks (same algorithm as values)
                    let mut dst_m_chunks: Vec<(VReg, usize)> = Vec::new();
                    let mut mf_idx = 0usize;
                    let mut mf_consumed = 0usize;
                    for chunk_i in 0..n_dst_chunks {
                        let chunk_width = if chunk_i == n_dst_chunks - 1 {
                            let rem = total_width % 64;
                            if rem == 0 { 64 } else { rem }
                        } else {
                            64
                        };
                        let mut acc = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm { dst: acc, value: 0 });
                        let mut ap = 0usize;
                        while ap < chunk_width && mf_idx < mask_flat.len() {
                            let (fv, fw) = mask_flat[mf_idx];
                            let rem = fw - mf_consumed;
                            let take = rem.min(chunk_width - ap);
                            let mut piece = fv;
                            if mf_consumed > 0 {
                                let s = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShrImm {
                                    dst: s,
                                    src: piece,
                                    imm: mf_consumed as u8,
                                });
                                piece = s;
                            }
                            if take < 64 {
                                let m = ctx.alloc_vreg(SpillDesc::transient());
                                ctx.emit_and_imm(block, m, piece, mask_for_width(take));
                                piece = m;
                            }
                            if ap > 0 {
                                let s = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShlImm {
                                    dst: s,
                                    src: piece,
                                    imm: ap as u8,
                                });
                                let mg = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Or {
                                    dst: mg,
                                    lhs: acc,
                                    rhs: s,
                                });
                                acc = mg;
                            } else {
                                let mg = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Or {
                                    dst: mg,
                                    lhs: acc,
                                    rhs: piece,
                                });
                                acc = mg;
                            }
                            ap += take;
                            mf_consumed += take;
                            if mf_consumed >= fw {
                                mf_idx += 1;
                                mf_consumed = 0;
                            }
                        }
                        dst_m_chunks.push((acc, chunk_width));
                    }
                    ctx.set_mask(*dst, dst_m_chunks[0].0);
                    ctx.wide_masks.insert(*dst, dst_m_chunks);
                }
            }
        }

        SIRInstruction::Slice(dst, src, bit_offset, width) => {
            let dst_vreg = ctx.reg_map.get(*dst);
            let src_width = ctx.sir_width(src);

            // If src has a known sim-state address (from a preceding Load/Store)
            // load directly from memory. This handles partial Stores that
            // updated memory without rewriting the source register's VRegs.
            if let Some((addr, source_bit_offset)) = ctx.reg_addrs.get(src).cloned() {
                let slice_bit_offset = source_bit_offset + *bit_offset;
                let value_base = ctx.byte_offset(&addr, 0);
                let value_chunks =
                    lower_static_wide_load_chunks(ctx, block, value_base, slice_bit_offset, *width);
                if *width <= 64 {
                    ctx.emit_mov(block, dst_vreg, value_chunks[0].0);
                } else {
                    ctx.set_wide_chunks(*dst, value_chunks);
                }

                if ctx.four_state {
                    let mask_chunks = if ctx.is_4state_var(&addr) {
                        let mask_base = ctx.mask_byte_offset(&addr, 0);
                        lower_static_wide_load_chunks(
                            ctx,
                            block,
                            mask_base,
                            slice_bit_offset,
                            *width,
                        )
                    } else {
                        let n_chunks = ISelContext::num_chunks(*width).max(1);
                        let mut chunks = Vec::with_capacity(n_chunks);
                        for index in 0..n_chunks {
                            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                            block.push(MInst::LoadImm {
                                dst: zero,
                                value: 0,
                            });
                            let chunk_width = width.saturating_sub(index * 64).min(64);
                            chunks.push((zero, chunk_width));
                        }
                        chunks
                    };
                    ctx.set_mask(*dst, mask_chunks[0].0);
                    if *width > 64 {
                        ctx.wide_masks.insert(*dst, mask_chunks);
                    }
                }
                return;
            }

            if *width <= 64 && src_width <= 64 {
                let src_vreg = ctx.reg_map.get(*src);
                if *bit_offset == 0 && *width == src_width {
                    ctx.emit_mov(block, dst_vreg, src_vreg);
                } else if *bit_offset == 0 {
                    let mask = mask_for_width(*width);
                    ctx.emit_and_imm(block, dst_vreg, src_vreg, mask);
                } else {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: shifted,
                        src: src_vreg,
                        imm: *bit_offset as u8,
                    });
                    let mask = mask_for_width(*width);
                    ctx.emit_and_imm(block, dst_vreg, shifted, mask);
                }
            } else if *width <= 64 {
                // Narrow slice from wide source
                let src_chunks = ctx.get_wide_chunks(src, block);
                let chunk_idx = *bit_offset / 64;
                let intra_bit = *bit_offset % 64;
                let main = ctx.wide_chunk_or_zero(&src_chunks, chunk_idx, block);

                if intra_bit == 0 {
                    let mask = mask_for_width(*width);
                    ctx.emit_and_imm(block, dst_vreg, main, mask);
                } else if intra_bit + *width <= 64 {
                    // Fits in one chunk after shift
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: shifted,
                        src: main,
                        imm: intra_bit as u8,
                    });
                    let mask = mask_for_width(*width);
                    ctx.emit_and_imm(block, dst_vreg, shifted, mask);
                } else {
                    // Crosses chunk boundary
                    let lo = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShrImm {
                        dst: lo,
                        src: main,
                        imm: intra_bit as u8,
                    });
                    let upper = ctx.wide_chunk_or_zero(&src_chunks, chunk_idx + 1, block);
                    let hi = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShlImm {
                        dst: hi,
                        src: upper,
                        imm: (64 - intra_bit) as u8,
                    });
                    let combined = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or {
                        dst: combined,
                        lhs: lo,
                        rhs: hi,
                    });
                    let mask = mask_for_width(*width);
                    ctx.emit_and_imm(block, dst_vreg, combined, mask);
                }
            } else {
                // Wide slice: extract bits from a wide source.
                // Get source chunks, then extract the requested range.
                let src_chunks = ctx.get_wide_chunks(src, block);
                let dst_n_chunks = ISelContext::num_chunks(*width);
                let chunk_start = *bit_offset / 64;
                let intra_bit = *bit_offset % 64;

                let mut dst_chunks = Vec::with_capacity(dst_n_chunks);
                for i in 0..dst_n_chunks {
                    let src_idx = chunk_start + i;
                    let main = ctx.wide_chunk_or_zero(&src_chunks, src_idx, block);

                    if intra_bit == 0 {
                        dst_chunks.push((main, 64));
                    } else {
                        // Cross-chunk: combine bits from src[src_idx] and src[src_idx+1]
                        let lo = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShrImm {
                            dst: lo,
                            src: main,
                            imm: intra_bit as u8,
                        });
                        let upper = ctx.wide_chunk_or_zero(&src_chunks, src_idx + 1, block);
                        let hi = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShlImm {
                            dst: hi,
                            src: upper,
                            imm: (64 - intra_bit) as u8,
                        });
                        let combined = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: combined,
                            lhs: lo,
                            rhs: hi,
                        });
                        dst_chunks.push((combined, 64));
                    }
                }

                // Mask the top chunk to the exact width
                let top_bits = *width % 64;
                if top_bits != 0 && !dst_chunks.is_empty() {
                    let last_idx = dst_chunks.len() - 1;
                    let (last_vreg, _) = dst_chunks[last_idx];
                    let masked = ctx.alloc_vreg(SpillDesc::transient());
                    ctx.emit_and_imm(block, masked, last_vreg, mask_for_width(top_bits));
                    dst_chunks[last_idx] = (masked, top_bits);
                }

                ctx.set_wide_chunks(*dst, dst_chunks);
            }

            if ctx.four_state {
                lower_slice_mask(ctx, block, *dst, *src, *bit_offset, *width);
            }
        }
    }
}

fn match_guarded_cmp_select_cond(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    sir_block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
    sir_defs: &HashMap<RegisterId, usize>,
    cond: RegisterId,
) -> Option<(VReg, VReg, VReg, CmpKind)> {
    let &cond_idx = sir_defs.get(&cond)?;
    let SIRInstruction::Binary(_, lhs, BinaryOp::LogicAnd, rhs) = sir_block.instructions[cond_idx]
    else {
        return None;
    };
    if let Some((cmp_lhs, cmp_rhs, kind)) = match_cmp_sir_value(ctx, sir_block, sir_defs, lhs) {
        let guard = lower_sir_bool_value(ctx, block, rhs)?;
        return Some((guard, cmp_lhs, cmp_rhs, kind));
    }
    if let Some((cmp_lhs, cmp_rhs, kind)) = match_cmp_sir_value(ctx, sir_block, sir_defs, rhs) {
        let guard = lower_sir_bool_value(ctx, block, lhs)?;
        return Some((guard, cmp_lhs, cmp_rhs, kind));
    }
    None
}

fn match_cmp_sir_value(
    ctx: &ISelContext,
    sir_block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
    sir_defs: &HashMap<RegisterId, usize>,
    reg: RegisterId,
) -> Option<(VReg, VReg, CmpKind)> {
    let &idx = sir_defs.get(&reg)?;
    let SIRInstruction::Binary(_, lhs, op, rhs) = sir_block.instructions[idx] else {
        return None;
    };
    let kind = match op {
        BinaryOp::Eq | BinaryOp::EqWildcard => CmpKind::Eq,
        BinaryOp::Ne | BinaryOp::NeWildcard => CmpKind::Ne,
        BinaryOp::LtU => CmpKind::LtU,
        BinaryOp::LtS => CmpKind::LtS,
        BinaryOp::LeU => CmpKind::LeU,
        BinaryOp::LeS => CmpKind::LeS,
        BinaryOp::GtU => CmpKind::GtU,
        BinaryOp::GtS => CmpKind::GtS,
        BinaryOp::GeU => CmpKind::GeU,
        BinaryOp::GeS => CmpKind::GeS,
        _ => return None,
    };
    if ctx.sir_width(&lhs) > 64
        || ctx.sir_width(&rhs) > 64
        || ctx.wide_regs.contains_key(&lhs)
        || ctx.wide_regs.contains_key(&rhs)
    {
        return None;
    }
    Some((ctx.reg_map.get(lhs), ctx.reg_map.get(rhs), kind))
}

fn lower_sir_bool_value(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    reg: RegisterId,
) -> Option<VReg> {
    if ctx.sir_width(&reg) > 64 {
        return None;
    }
    let raw = if ctx.wide_regs.contains_key(&reg) {
        ctx.get_wide_chunks(&reg, block)[0].0
    } else {
        ctx.reg_map.get(reg)
    };
    Some(lower_bool_value(ctx, block, raw))
}

fn try_lower_concat_of_muxes(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    args: &[RegisterId],
    sir_block: &crate::ir::BasicBlock<RegionedAbsoluteAddr>,
    sir_defs: &HashMap<RegisterId, usize>,
) -> bool {
    if ctx.four_state || args.len() < 2 {
        return false;
    }

    let total_width = args.iter().map(|arg| ctx.sir_width(arg)).sum::<usize>();
    if total_width == 0 || total_width != ctx.sir_width(&dst) {
        return false;
    }

    let mut cond = None;
    let mut then_parts = Vec::with_capacity(args.len());
    let mut else_parts = Vec::with_capacity(args.len());

    for &arg in args {
        let Some(&idx) = sir_defs.get(&arg) else {
            return false;
        };
        let SIRInstruction::Mux(mux_dst, mux_cond, then_val, else_val) =
            sir_block.instructions[idx]
        else {
            return false;
        };
        if mux_dst != arg {
            return false;
        }
        if let Some(existing_cond) = cond {
            if existing_cond != mux_cond {
                return false;
            }
        } else {
            cond = Some(mux_cond);
        }
        let width = ctx.sir_width(&arg);
        if ctx.sir_width(&then_val) < width || ctx.sir_width(&else_val) < width {
            return false;
        }
        then_parts.push((then_val, width));
        else_parts.push((else_val, width));
    }

    let cond = cond.expect("non-empty mux concat must have a condition");
    let (cond_vreg, _) = lower_mux_condition_state(ctx, block, cond);

    let then_chunks = lower_concat_parts_to_chunks(ctx, block, &then_parts, total_width);
    let else_chunks = lower_concat_parts_to_chunks(ctx, block, &else_parts, total_width);
    let result_chunks = lower_mux_chunk_blend(
        ctx,
        block,
        cond_vreg,
        &then_chunks,
        &else_chunks,
        total_width,
    );

    if total_width <= 64 {
        let dst_vreg = ctx.reg_map.get(dst);
        if let Some(&(result, _)) = result_chunks.first() {
            ctx.emit_mov(block, dst_vreg, result);
            ctx.known_bits.insert(dst_vreg, total_width);
        }
    } else {
        ctx.set_wide_chunks(dst, result_chunks);
    }
    true
}

fn lower_concat_parts_to_chunks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    parts: &[(RegisterId, usize)],
    total_width: usize,
) -> Vec<(VReg, usize)> {
    let n_dst_chunks = ISelContext::num_chunks(total_width);
    let mut flat_bits: Vec<(VReg, usize)> = Vec::with_capacity(parts.len());
    for &(reg, width) in parts.iter().rev() {
        if width > 64 || ctx.wide_regs.contains_key(&reg) {
            let chunks = ctx.get_wide_chunks(&reg, block);
            let mut remaining = width;
            for (chunk, chunk_width) in chunks {
                if remaining == 0 {
                    break;
                }
                let take = chunk_width.min(remaining);
                flat_bits.push((chunk, take));
                remaining -= take;
            }
        } else {
            let vreg = ctx.reg_map.get(reg);
            flat_bits.push((vreg, width));
        }
    }

    let mut dst_chunks = Vec::with_capacity(n_dst_chunks);
    let mut flat_idx = 0usize;
    let mut flat_consumed = 0usize;

    for chunk_i in 0..n_dst_chunks {
        let chunk_width = if chunk_i == n_dst_chunks - 1 {
            let rem = total_width % 64;
            if rem == 0 { 64 } else { rem }
        } else {
            64
        };

        let mut acc = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm { dst: acc, value: 0 });
        let mut acc_pos = 0usize;

        while acc_pos < chunk_width && flat_idx < flat_bits.len() {
            let (fv, fw) = flat_bits[flat_idx];
            let remaining_in_flat = fw - flat_consumed;
            let need = chunk_width - acc_pos;
            let take = remaining_in_flat.min(need);

            let mut piece = fv;
            if flat_consumed > 0 {
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: shifted,
                    src: piece,
                    imm: flat_consumed as u8,
                });
                piece = shifted;
            }
            if take < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, piece, mask_for_width(take));
                piece = masked;
            }

            if acc_pos > 0 {
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShlImm {
                    dst: shifted,
                    src: piece,
                    imm: acc_pos as u8,
                });
                piece = shifted;
            }

            let merged = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: merged,
                lhs: acc,
                rhs: piece,
            });
            acc = merged;

            acc_pos += take;
            flat_consumed += take;
            if flat_consumed >= fw {
                flat_idx += 1;
                flat_consumed = 0;
            }
        }

        dst_chunks.push((acc, chunk_width));
    }

    dst_chunks
}

fn lower_mux_chunk_blend(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    cond_vreg: VReg,
    then_chunks: &[(VReg, usize)],
    else_chunks: &[(VReg, usize)],
    total_width: usize,
) -> Vec<(VReg, usize)> {
    let n_chunks = ISelContext::num_chunks(total_width);
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let cond_bc_raw = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Sub {
        dst: cond_bc_raw,
        lhs: zero,
        rhs: cond_vreg,
    });

    let mut result_chunks = Vec::with_capacity(n_chunks);
    for i in 0..n_chunks {
        let chunk_width = if i == n_chunks - 1 {
            let rem = total_width % 64;
            if rem == 0 { 64 } else { rem }
        } else {
            64
        };
        let tv = then_chunks.get(i).map(|&(v, _)| v).unwrap_or(zero);
        let ev = else_chunks.get(i).map(|&(v, _)| v).unwrap_or(zero);
        let cond_bc = if chunk_width < 64 {
            let masked = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked, cond_bc_raw, mask_for_width(chunk_width));
            masked
        } else {
            cond_bc_raw
        };

        let result = if tv == ev {
            tv
        } else {
            let diff = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Xor {
                dst: diff,
                lhs: tv,
                rhs: ev,
            });
            let selected_diff = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: selected_diff,
                lhs: diff,
                rhs: cond_bc,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Xor {
                dst: res,
                lhs: ev,
                rhs: selected_diff,
            });
            res
        };
        result_chunks.push((result, chunk_width));
    }

    result_chunks
}

// ────────────────────────────────────────────────────────────────
// Wide (>64-bit) operation lowering via multi-word chunks
// ────────────────────────────────────────────────────────────────

fn wide_sign_bit(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    chunks: &[(VReg, usize)],
    width: usize,
) -> VReg {
    if width == 0 {
        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm {
            dst: zero,
            value: 0,
        });
        return zero;
    }
    let sign_index = width - 1;
    let source = ctx.wide_chunk_or_zero(chunks, sign_index / 64, block);
    let shifted = if sign_index % 64 == 0 {
        source
    } else {
        let shifted = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::ShrImm {
            dst: shifted,
            src: source,
            imm: (sign_index % 64) as u8,
        });
        shifted
    };
    let sign = ctx.alloc_vreg(SpillDesc::transient());
    ctx.emit_and_imm(block, sign, shifted, 1);
    sign
}

fn sign_extend_wide_chunks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    chunks: &[(VReg, usize)],
    width: usize,
    num_chunks: usize,
) -> Vec<VReg> {
    let sign = wide_sign_bit(ctx, block, chunks, width);
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let all_ones = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
    block.push(MInst::LoadImm {
        dst: all_ones,
        value: u64::MAX,
    });
    let fill = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: fill,
        cond: sign,
        true_val: all_ones,
        false_val: zero,
    });
    let source_chunks = width.div_ceil(64);
    let top_bits = width % 64;
    let mut extended = Vec::with_capacity(num_chunks);
    for index in 0..num_chunks {
        if index >= source_chunks {
            extended.push(fill);
            continue;
        }
        let raw = ctx.wide_chunk_or_zero(chunks, index, block);
        if index + 1 != source_chunks || top_bits == 0 {
            extended.push(raw);
            continue;
        }
        let low_mask = mask_for_width(top_bits);
        let low = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, low, raw, low_mask);
        let high = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, high, fill, !low_mask);
        let combined = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: combined,
            lhs: low,
            rhs: high,
        });
        extended.push(combined);
    }
    extended
}

fn conditional_negate_wide_chunks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    chunks: &[VReg],
    negate: VReg,
    num_chunks: usize,
) -> Vec<VReg> {
    let one = ctx.alloc_vreg(SpillDesc::remat(1));
    block.push(MInst::LoadImm { dst: one, value: 1 });
    let mut carry = one;
    let mut negated = Vec::with_capacity(num_chunks);
    for index in 0..num_chunks {
        let inverted = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::BitNot {
            dst: inverted,
            src: chunks[index],
        });
        let sum = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Add {
            dst: sum,
            lhs: inverted,
            rhs: carry,
        });
        let next_carry = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Cmp {
            dst: next_carry,
            lhs: sum,
            rhs: inverted,
            kind: CmpKind::LtU,
        });
        negated.push(sum);
        carry = next_carry;
    }
    (0..num_chunks)
        .map(|index| {
            let selected = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: selected,
                cond: negate,
                true_val: negated[index],
                false_val: chunks[index],
            });
            selected
        })
        .collect()
}

/// Lower a binary operation on wide (>64-bit) values.
/// Supports: And, Or, Xor (chunk-wise) and Shl (multi-word shift).
fn lower_wide_binary(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: RegisterId,
    op: &BinaryOp,
    rhs: RegisterId,
) {
    let d_width = ctx.sir_width(&dst);
    let lhs_width = ctx.sir_width(&lhs);
    let rhs_width = ctx.sir_width(&rhs);
    // For comparisons and logic ops, the result may be narrow (1 bit)
    // but we need to process all chunks of the wider operand.
    let operation_width = d_width.max(lhs_width).max(rhs_width);
    let n_chunks = ISelContext::num_chunks(operation_width);

    if ctx.four_state && matches!(op, BinaryOp::EqWildcard | BinaryOp::NeWildcard) {
        lower_wide_wildcard_compare(ctx, block, dst, lhs, op, rhs, operation_width);
        return;
    }

    match op {
        // Chunk-wise operations: apply to each 64-bit chunk independently
        BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);
            let mut dst_chunks = Vec::with_capacity(n_chunks);

            for i in 0..n_chunks {
                let l = lhs_chunks.get(i).map(|c| c.0).unwrap_or_else(|| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                });
                let r = rhs_chunks.get(i).map(|c| c.0).unwrap_or_else(|| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                });
                let d = ctx.alloc_vreg(SpillDesc::transient());
                match op {
                    BinaryOp::And => block.push(MInst::And {
                        dst: d,
                        lhs: l,
                        rhs: r,
                    }),
                    BinaryOp::Or => block.push(MInst::Or {
                        dst: d,
                        lhs: l,
                        rhs: r,
                    }),
                    BinaryOp::Xor => block.push(MInst::Xor {
                        dst: d,
                        lhs: l,
                        rhs: r,
                    }),
                    _ => unreachable!(),
                }
                dst_chunks.push((d, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide left shift by a scalar amount.
        // If the shift amount is a known constant (common after loop unrolling),
        // compute chunk assignments directly without runtime select chains.
        BinaryOp::Shl => {
            // (debug removed)
            let src_chunks = ctx.get_wide_chunks(&lhs, block);
            let n_src = src_chunks.len();

            if let Some(&amount) = ctx.consts.get(&rhs) {
                // Constant shift: compute each chunk statically
                let cs = (amount / 64) as usize; // chunk shift
                let is = (amount % 64) as u8; // intra-chunk shift

                let mut dst_chunks = Vec::with_capacity(n_chunks);
                for i in 0..n_chunks {
                    if i < cs {
                        let z = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm { dst: z, value: 0 });
                        dst_chunks.push((z, 64));
                    } else {
                        let src_idx = i - cs;
                        let main_vreg = if src_idx < n_src {
                            src_chunks[src_idx].0
                        } else {
                            let z = ctx.alloc_vreg(SpillDesc::remat(0));
                            block.push(MInst::LoadImm { dst: z, value: 0 });
                            z
                        };

                        if is == 0 {
                            dst_chunks.push((main_vreg, 64));
                        } else {
                            // main_part = src[src_idx] << is
                            let main_shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm {
                                dst: main_shifted,
                                src: main_vreg,
                                imm: is,
                            });

                            // carry from lower chunk: src[src_idx-1] >> (64 - is)
                            if src_idx > 0 && (src_idx - 1) < n_src {
                                let carry_vreg = src_chunks[src_idx - 1].0;
                                let carry_shifted = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShrImm {
                                    dst: carry_shifted,
                                    src: carry_vreg,
                                    imm: 64 - is,
                                });
                                let combined = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Or {
                                    dst: combined,
                                    lhs: main_shifted,
                                    rhs: carry_shifted,
                                });
                                dst_chunks.push((combined, 64));
                            } else {
                                dst_chunks.push((main_shifted, 64));
                            }
                        }
                    }
                }
                ctx.set_wide_chunks(dst, dst_chunks);
            } else {
                // Runtime left shift: select chain + carry propagation.
                lower_wide_runtime_shift(
                    ctx,
                    block,
                    dst,
                    &lhs,
                    &rhs,
                    n_chunks,
                    ShiftDir::Left,
                    false,
                );
            }
        }

        // Wide addition with carry chain
        BinaryOp::Add => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            let mut carry: Option<VReg> = None;

            for i in 0..n_chunks {
                let l = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let r = ctx.wide_chunk_or_zero(&rhs_chunks, i, block);

                if let Some(cin) = carry {
                    // s1 = l + r
                    let s1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: s1,
                        lhs: l,
                        rhs: r,
                    });
                    // c1 = (s1 < l) unsigned
                    let c1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c1,
                        lhs: s1,
                        rhs: l,
                        kind: CmpKind::LtU,
                    });
                    // s2 = s1 + cin
                    let s2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: s2,
                        lhs: s1,
                        rhs: cin,
                    });
                    // c2 = (s2 < s1) unsigned
                    let c2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c2,
                        lhs: s2,
                        rhs: s1,
                        kind: CmpKind::LtU,
                    });
                    // carry = c1 | c2
                    let cout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or {
                        dst: cout,
                        lhs: c1,
                        rhs: c2,
                    });
                    carry = Some(cout);
                    dst_chunks.push((s2, 64));
                } else {
                    // s = l + r
                    let s = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: s,
                        lhs: l,
                        rhs: r,
                    });
                    // carry = (s < l) unsigned
                    let cout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: cout,
                        lhs: s,
                        rhs: l,
                        kind: CmpKind::LtU,
                    });
                    carry = Some(cout);
                    dst_chunks.push((s, 64));
                }
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide subtraction with borrow chain
        BinaryOp::Sub => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            let mut borrow: Option<VReg> = None;

            for i in 0..n_chunks {
                let l = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let r = ctx.wide_chunk_or_zero(&rhs_chunks, i, block);

                if let Some(bin) = borrow {
                    // d1 = l - r
                    let d1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Sub {
                        dst: d1,
                        lhs: l,
                        rhs: r,
                    });
                    // b1 = (r > l) unsigned
                    let b1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: b1,
                        lhs: r,
                        rhs: l,
                        kind: CmpKind::GtU,
                    });
                    // d2 = d1 - bin
                    let d2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Sub {
                        dst: d2,
                        lhs: d1,
                        rhs: bin,
                    });
                    // b2 = (bin > d1) unsigned
                    let b2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: b2,
                        lhs: bin,
                        rhs: d1,
                        kind: CmpKind::GtU,
                    });
                    // borrow = b1 | b2
                    let bout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or {
                        dst: bout,
                        lhs: b1,
                        rhs: b2,
                    });
                    borrow = Some(bout);
                    dst_chunks.push((d2, 64));
                } else {
                    // d = l - r
                    let d = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Sub {
                        dst: d,
                        lhs: l,
                        rhs: r,
                    });
                    // borrow = (r > l) unsigned
                    let bout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: bout,
                        lhs: r,
                        rhs: l,
                        kind: CmpKind::GtU,
                    });
                    borrow = Some(bout);
                    dst_chunks.push((d, 64));
                }
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide equality/inequality: chunk-wise AND/OR of per-chunk comparisons
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            let is_eq = matches!(op, BinaryOp::Eq | BinaryOp::EqWildcard);
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);

            let init = ctx.alloc_vreg(SpillDesc::remat(if is_eq { 1 } else { 0 }));
            block.push(MInst::LoadImm {
                dst: init,
                value: if is_eq { 1 } else { 0 },
            });
            let mut cond = init;

            for i in 0..n_chunks {
                let l = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let r = ctx.wide_chunk_or_zero(&rhs_chunks, i, block);
                let eq = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: eq,
                    lhs: l,
                    rhs: r,
                    kind: CmpKind::Eq,
                });
                let next = ctx.alloc_vreg(SpillDesc::transient());
                if is_eq {
                    block.push(MInst::And {
                        dst: next,
                        lhs: cond,
                        rhs: eq,
                    });
                } else {
                    // ne: accumulate OR of (chunk != chunk)
                    let neq = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: neq,
                        lhs: l,
                        rhs: r,
                        kind: CmpKind::Ne,
                    });
                    block.push(MInst::Or {
                        dst: next,
                        lhs: cond,
                        rhs: neq,
                    });
                }
                cond = next;
            }
            // Result is a 1-bit value in chunk 0, rest zero
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((cond, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide unsigned comparisons: compare from MSB chunk down
        BinaryOp::LtU | BinaryOp::LeU | BinaryOp::GtU | BinaryOp::GeU => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);

            // Init: Le/Ge → 1 (true when equal), Lt/Gt → 0 (false when equal)
            let init_val = if matches!(op, BinaryOp::LeU | BinaryOp::GeU) {
                1u64
            } else {
                0u64
            };
            let init = ctx.alloc_vreg(SpillDesc::remat(init_val));
            block.push(MInst::LoadImm {
                dst: init,
                value: init_val,
            });
            let mut res = init;

            let cmp_kind = match op {
                BinaryOp::LtU | BinaryOp::LeU => CmpKind::LtU,
                BinaryOp::GtU | BinaryOp::GeU => CmpKind::GtU,
                _ => unreachable!(),
            };

            // Process from LSB to MSB; each chunk: if equal keep previous, else use this chunk's cmp
            for i in 0..n_chunks {
                let l = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let r = ctx.wide_chunk_or_zero(&rhs_chunks, i, block);
                let eq = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: eq,
                    lhs: l,
                    rhs: r,
                    kind: CmpKind::Eq,
                });
                let cmp = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: cmp,
                    lhs: l,
                    rhs: r,
                    kind: cmp_kind,
                });
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: next,
                    cond: eq,
                    true_val: res,
                    false_val: cmp,
                });
                res = next;
            }

            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((res, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide signed comparisons: compare MSB chunk signed, lower chunks unsigned
        BinaryOp::LtS | BinaryOp::LeS | BinaryOp::GtS | BinaryOp::GeS => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);
            let top_bits = operation_width - (n_chunks - 1) * 64;

            let init_val = if matches!(op, BinaryOp::LeS | BinaryOp::GeS) {
                1u64
            } else {
                0u64
            };
            let init = ctx.alloc_vreg(SpillDesc::remat(init_val));
            block.push(MInst::LoadImm {
                dst: init,
                value: init_val,
            });
            let mut res = init;

            let unsigned_kind = match op {
                BinaryOp::LtS | BinaryOp::LeS => CmpKind::LtU,
                BinaryOp::GtS | BinaryOp::GeS => CmpKind::GtU,
                _ => unreachable!(),
            };
            let signed_kind = match op {
                BinaryOp::LtS | BinaryOp::LeS => CmpKind::LtS,
                BinaryOp::GtS | BinaryOp::GeS => CmpKind::GtS,
                _ => unreachable!(),
            };

            for i in 0..n_chunks {
                let l = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let r = ctx.wide_chunk_or_zero(&rhs_chunks, i, block);
                let eq = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: eq,
                    lhs: l,
                    rhs: r,
                    kind: CmpKind::Eq,
                });
                // MSB chunk uses signed comparison, lower chunks use unsigned
                let (l, r, kind) = if i == n_chunks - 1 {
                    // Wide values keep unused top-chunk bits clear. For a
                    // non-64-multiple width the logical sign is therefore not
                    // physical bit 63; extend it before the signed compare.
                    (
                        sign_extend_scalar(ctx, block, l, top_bits),
                        sign_extend_scalar(ctx, block, r, top_bits),
                        signed_kind,
                    )
                } else {
                    (l, r, unsigned_kind)
                };
                let cmp = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: cmp,
                    lhs: l,
                    rhs: r,
                    kind,
                });
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: next,
                    cond: eq,
                    true_val: res,
                    false_val: cmp,
                });
                res = next;
            }

            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((res, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide right shifts (logical and arithmetic)
        BinaryOp::Shr | BinaryOp::Sar => {
            let is_sar = matches!(op, BinaryOp::Sar);
            let src_chunks = ctx.get_wide_chunks(&lhs, block);
            let n_src = src_chunks.len();

            if let Some(&amount) = ctx.consts.get(&rhs) {
                // Constant shift
                let cs = (amount / 64) as usize; // chunk shift
                let is = (amount % 64) as u8; // intra-chunk shift

                let mut dst_chunks = Vec::with_capacity(n_chunks);
                for i in 0..n_chunks {
                    let src_idx = i + cs;
                    let main_vreg = if src_idx < n_src {
                        src_chunks[src_idx].0
                    } else if is_sar {
                        // SAR: fill with sign extension from MSB chunk
                        let msb = src_chunks[n_src - 1].0;
                        let sign = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::SarImm {
                            dst: sign,
                            src: msb,
                            imm: 63,
                        });
                        sign
                    } else {
                        let z = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm { dst: z, value: 0 });
                        z
                    };

                    if is == 0 {
                        dst_chunks.push((main_vreg, 64));
                    } else {
                        // main_part = src[src_idx] >> is  (logical for SHR, logical here too — sign handled by carry)
                        let main_shifted = ctx.alloc_vreg(SpillDesc::transient());
                        if is_sar && i == n_chunks - 1 {
                            // MSB chunk of SAR: arithmetic shift
                            block.push(MInst::SarImm {
                                dst: main_shifted,
                                src: main_vreg,
                                imm: is,
                            });
                        } else {
                            block.push(MInst::ShrImm {
                                dst: main_shifted,
                                src: main_vreg,
                                imm: is,
                            });
                        }

                        // carry from upper chunk: src[src_idx+1] << (64 - is)
                        let upper_idx = src_idx + 1;
                        if upper_idx < n_src {
                            let carry_vreg = src_chunks[upper_idx].0;
                            let carry_shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm {
                                dst: carry_shifted,
                                src: carry_vreg,
                                imm: 64 - is,
                            });
                            let combined = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or {
                                dst: combined,
                                lhs: main_shifted,
                                rhs: carry_shifted,
                            });
                            dst_chunks.push((combined, 64));
                        } else if is_sar && i < n_chunks - 1 {
                            // SAR: carry from sign-extended chunk
                            let msb = src_chunks[n_src - 1].0;
                            let sign = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::SarImm {
                                dst: sign,
                                src: msb,
                                imm: 63,
                            });
                            let carry_shifted = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShlImm {
                                dst: carry_shifted,
                                src: sign,
                                imm: 64 - is,
                            });
                            let combined = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::Or {
                                dst: combined,
                                lhs: main_shifted,
                                rhs: carry_shifted,
                            });
                            dst_chunks.push((combined, 64));
                        } else {
                            dst_chunks.push((main_shifted, 64));
                        }
                    }
                }
                ctx.set_wide_chunks(dst, dst_chunks);
            } else {
                // Runtime right shift: select chain + carry propagation.
                let dir = if is_sar {
                    ShiftDir::ArithRight
                } else {
                    ShiftDir::Right
                };
                lower_wide_runtime_shift(ctx, block, dst, &lhs, &rhs, n_chunks, dir, is_sar);
            }
        }

        // Wide logical operations (result is 1-bit)
        BinaryOp::LogicAnd | BinaryOp::LogicOr => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);

            // Reduce lhs to bool: any chunk non-zero?
            let lhs_bool = wide_reduce_or(ctx, block, &lhs_chunks, n_chunks);
            // Reduce rhs to bool
            let rhs_bool = wide_reduce_or(ctx, block, &rhs_chunks, n_chunks);

            let result = ctx.alloc_vreg(SpillDesc::transient());
            match op {
                BinaryOp::LogicAnd => block.push(MInst::And {
                    dst: result,
                    lhs: lhs_bool,
                    rhs: rhs_bool,
                }),
                BinaryOp::LogicOr => block.push(MInst::Or {
                    dst: result,
                    lhs: lhs_bool,
                    rhs: rhs_bool,
                }),
                _ => unreachable!(),
            }

            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((result, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide multiplication: schoolbook O(n²) using UMulHi for 64×64→128.
        BinaryOp::Mul => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);

            // Accumulator: n_chunks of VRegs initialized to 0
            let mut acc: Vec<VReg> = (0..n_chunks)
                .map(|_| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                })
                .collect();

            for i in 0..n_chunks {
                let a_i = ctx.wide_chunk_or_zero(&lhs_chunks, i, block);
                let mut carry = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: carry,
                    value: 0,
                });

                for j in 0..n_chunks {
                    let k = i + j;
                    if k >= n_chunks {
                        break;
                    }

                    let b_j = ctx.wide_chunk_or_zero(&rhs_chunks, j, block);

                    // lo = a_i * b_j, hi = umulhi(a_i, b_j)
                    let lo = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Mul {
                        dst: lo,
                        lhs: a_i,
                        rhs: b_j,
                    });
                    let hi = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::UMulHi {
                        dst: hi,
                        lhs: a_i,
                        rhs: b_j,
                    });

                    // sum1 = acc[k] + lo
                    let sum1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: sum1,
                        lhs: acc[k],
                        rhs: lo,
                    });
                    let c1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c1,
                        lhs: sum1,
                        rhs: acc[k],
                        kind: CmpKind::LtU,
                    });

                    // sum2 = sum1 + carry
                    let sum2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: sum2,
                        lhs: sum1,
                        rhs: carry,
                    });
                    let c2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c2,
                        lhs: sum2,
                        rhs: sum1,
                        kind: CmpKind::LtU,
                    });

                    acc[k] = sum2;

                    // carry = hi + c1 + c2
                    let carry1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: carry1,
                        lhs: hi,
                        rhs: c1,
                    });
                    let new_carry = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: new_carry,
                        lhs: carry1,
                        rhs: c2,
                    });
                    carry = new_carry;
                }
            }

            let dst_chunks: Vec<(VReg, usize)> = acc.into_iter().map(|v| (v, 64)).collect();
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide division/remainder: bit-by-bit restoring division.
        BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS => {
            let lhs_chunks = ctx.get_wide_chunks(&lhs, block);
            let rhs_chunks = ctx.get_wide_chunks(&rhs, block);
            let signed = matches!(op, BinaryOp::DivS | BinaryOp::RemS);
            let lhs_negative = wide_sign_bit(ctx, block, &lhs_chunks, lhs_width);
            let rhs_negative = wide_sign_bit(ctx, block, &rhs_chunks, rhs_width);
            let normalized_lhs = if signed {
                let extended =
                    sign_extend_wide_chunks(ctx, block, &lhs_chunks, lhs_width, n_chunks);
                conditional_negate_wide_chunks(ctx, block, &extended, lhs_negative, n_chunks)
            } else {
                (0..n_chunks)
                    .map(|index| ctx.wide_chunk_or_zero(&lhs_chunks, index, block))
                    .collect()
            };
            let normalized_rhs = if signed {
                let extended =
                    sign_extend_wide_chunks(ctx, block, &rhs_chunks, rhs_width, n_chunks);
                conditional_negate_wide_chunks(ctx, block, &extended, rhs_negative, n_chunks)
            } else {
                (0..n_chunks)
                    .map(|index| ctx.wide_chunk_or_zero(&rhs_chunks, index, block))
                    .collect()
            };
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let mut divisor_or = zero;
            for &chunk in &normalized_rhs {
                let combined = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: combined,
                    lhs: divisor_or,
                    rhs: chunk,
                });
                divisor_or = combined;
            }
            let divisor_is_zero = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: divisor_is_zero,
                lhs: divisor_or,
                rhs: zero,
                kind: CmpKind::Eq,
            });
            let total_bits = operation_width;

            let mut q_chunks: Vec<VReg> = (0..n_chunks)
                .map(|_| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                })
                .collect();
            let mut rem_chunks: Vec<VReg> = (0..n_chunks)
                .map(|_| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                })
                .collect();

            for bit in (0..total_bits).rev() {
                let chunk_idx = bit / 64;
                let bit_idx = bit % 64;

                // remainder <<= 1
                for c in (0..n_chunks).rev() {
                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::ShlImm {
                        dst: shifted,
                        src: rem_chunks[c],
                        imm: 1,
                    });
                    if c > 0 {
                        let carry_bit = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::ShrImm {
                            dst: carry_bit,
                            src: rem_chunks[c - 1],
                            imm: 63,
                        });
                        let combined = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: combined,
                            lhs: shifted,
                            rhs: carry_bit,
                        });
                        rem_chunks[c] = combined;
                    } else {
                        rem_chunks[c] = shifted;
                    }
                }

                // remainder[0] |= (dividend[chunk_idx] >> bit_idx) & 1
                let dividend_chunk = normalized_lhs[chunk_idx];
                let extracted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: extracted,
                    src: dividend_chunk,
                    imm: bit_idx as u8,
                });
                let one_bit = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, one_bit, extracted, 1);
                let new_rem0 = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: new_rem0,
                    lhs: rem_chunks[0],
                    rhs: one_bit,
                });
                rem_chunks[0] = new_rem0;

                // if remainder >= divisor (chunk-wise unsigned comparison)
                let init_ge = ctx.alloc_vreg(SpillDesc::remat(1));
                block.push(MInst::LoadImm {
                    dst: init_ge,
                    value: 1,
                });
                let mut ge = init_ge;
                for (c, &rc) in rem_chunks.iter().enumerate() {
                    let dc = normalized_rhs[c];
                    let eq = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: eq,
                        lhs: rc,
                        rhs: dc,
                        kind: CmpKind::Eq,
                    });
                    let gt = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: gt,
                        lhs: rc,
                        rhs: dc,
                        kind: CmpKind::GeU,
                    });
                    let next_ge = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: next_ge,
                        cond: eq,
                        true_val: ge,
                        false_val: gt,
                    });
                    ge = next_ge;
                }

                // conditional: remainder -= divisor (wide sub with borrow)
                let mut borrow: Option<VReg> = None;
                for (c, rc) in rem_chunks.iter_mut().enumerate() {
                    let old_rc = *rc;
                    let dc = normalized_rhs[c];

                    let (diff, bout) = if let Some(bin) = borrow {
                        let d1 = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Sub {
                            dst: d1,
                            lhs: old_rc,
                            rhs: dc,
                        });
                        let b1 = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: b1,
                            lhs: dc,
                            rhs: old_rc,
                            kind: CmpKind::GtU,
                        });
                        let d2 = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Sub {
                            dst: d2,
                            lhs: d1,
                            rhs: bin,
                        });
                        let b2 = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: b2,
                            lhs: bin,
                            rhs: d1,
                            kind: CmpKind::GtU,
                        });
                        let bout = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: bout,
                            lhs: b1,
                            rhs: b2,
                        });
                        (d2, bout)
                    } else {
                        let d = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Sub {
                            dst: d,
                            lhs: old_rc,
                            rhs: dc,
                        });
                        let bout = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Cmp {
                            dst: bout,
                            lhs: dc,
                            rhs: old_rc,
                            kind: CmpKind::GtU,
                        });
                        (d, bout)
                    };

                    // select: if ge then subtracted else original
                    let new_rc = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: new_rc,
                        cond: ge,
                        true_val: diff,
                        false_val: old_rc,
                    });
                    *rc = new_rc;
                    borrow = Some(bout);
                }

                // quotient[chunk_idx] |= ge ? (1 << bit_idx) : 0
                let bit_mask = ctx.alloc_vreg(SpillDesc::remat(1u64 << bit_idx));
                block.push(MInst::LoadImm {
                    dst: bit_mask,
                    value: 1u64 << bit_idx,
                });
                let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: zero,
                    value: 0,
                });
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: masked,
                    cond: ge,
                    true_val: bit_mask,
                    false_val: zero,
                });
                let new_q = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: new_q,
                    lhs: q_chunks[chunk_idx],
                    rhs: masked,
                });
                q_chunks[chunk_idx] = new_q;
            }

            let magnitude = if matches!(op, BinaryOp::DivU | BinaryOp::DivS) {
                q_chunks
            } else {
                rem_chunks
            };
            let signed_result = if signed {
                let result_negative = if matches!(op, BinaryOp::DivS) {
                    let negative = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Xor {
                        dst: negative,
                        lhs: lhs_negative,
                        rhs: rhs_negative,
                    });
                    negative
                } else {
                    lhs_negative
                };
                conditional_negate_wide_chunks(ctx, block, &magnitude, result_negative, n_chunks)
            } else {
                magnitude
            };
            let mut result_chunks = Vec::with_capacity(n_chunks);
            for (index, chunk) in signed_result.into_iter().enumerate() {
                let defined = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: defined,
                    cond: divisor_is_zero,
                    true_val: zero,
                    false_val: chunk,
                });
                let top_bits = d_width % 64;
                let defined = if index + 1 == ISelContext::num_chunks(d_width) && top_bits != 0 {
                    let masked = ctx.alloc_vreg(SpillDesc::transient());
                    ctx.emit_and_imm(block, masked, defined, mask_for_width(top_bits));
                    masked
                } else {
                    defined
                };
                result_chunks.push(defined);
            }
            result_chunks.truncate(ISelContext::num_chunks(d_width));
            let dst_chunks: Vec<(VReg, usize)> = result_chunks
                .into_iter()
                .enumerate()
                .map(|(index, value)| {
                    let bits = if index + 1 == ISelContext::num_chunks(d_width) {
                        let top = d_width % 64;
                        if top == 0 { 64 } else { top }
                    } else {
                        64
                    };
                    (value, bits)
                })
                .collect();
            ctx.set_wide_chunks(dst, dst_chunks);
        }
    }

    // When the result is narrow (≤64 bits, e.g. comparison result),
    // sync chunk[0] back to the scalar reg_map so scalar Store paths
    // can read it.
    if d_width <= 64 {
        if let Some(chunks) = ctx.wide_regs.get(&dst) {
            let chunk0 = chunks[0].0;
            let scalar = ctx.reg_map.get(dst);
            if chunk0 != scalar {
                ctx.emit_alias_mov(block, scalar, chunk0);
            }
        }
    }
}

fn lower_wide_wildcard_compare(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: RegisterId,
    op: &BinaryOp,
    rhs: RegisterId,
    operation_width: usize,
) {
    let n_chunks = ISelContext::num_chunks(operation_width);
    let lhs_values = ctx.get_wide_chunks(&lhs, block);
    let rhs_values = ctx.get_wide_chunks(&rhs, block);
    let lhs_masks = get_wide_mask_chunks(ctx, block, &lhs, n_chunks);
    let rhs_masks = get_wide_mask_chunks(ctx, block, &rhs, n_chunks);
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let mut mismatch_bits = zero;
    let mut unknown_bits = zero;

    for index in 0..n_chunks {
        let lhs_value = ctx.wide_chunk_or_zero(&lhs_values, index, block);
        let rhs_value = ctx.wide_chunk_or_zero(&rhs_values, index, block);
        let lhs_mask = lhs_masks[index];
        let rhs_mask = rhs_masks[index];
        let chunk_width = (operation_width - index * 64).min(64);

        let not_rhs_mask = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::BitNot {
            dst: not_rhs_mask,
            src: rhs_mask,
        });
        let not_lhs_mask = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::BitNot {
            dst: not_lhs_mask,
            src: lhs_mask,
        });
        let known_compared = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: known_compared,
            lhs: not_rhs_mask,
            rhs: not_lhs_mask,
        });
        let value_diff = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Xor {
            dst: value_diff,
            lhs: lhs_value,
            rhs: rhs_value,
        });
        let mismatch = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: mismatch,
            lhs: value_diff,
            rhs: known_compared,
        });
        let lhs_unknown = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: lhs_unknown,
            lhs: lhs_mask,
            rhs: not_rhs_mask,
        });

        let (mismatch, lhs_unknown) = if chunk_width < 64 {
            let valid = mask_for_width(chunk_width);
            let masked_mismatch = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked_mismatch, mismatch, valid);
            let masked_unknown = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked_unknown, lhs_unknown, valid);
            (masked_mismatch, masked_unknown)
        } else {
            (mismatch, lhs_unknown)
        };

        let next_mismatch = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: next_mismatch,
            lhs: mismatch_bits,
            rhs: mismatch,
        });
        mismatch_bits = next_mismatch;
        let next_unknown = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: next_unknown,
            lhs: unknown_bits,
            rhs: lhs_unknown,
        });
        unknown_bits = next_unknown;
    }

    let has_mismatch = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: has_mismatch,
        lhs: mismatch_bits,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    let has_unknown = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: has_unknown,
        lhs: unknown_bits,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    let value = if matches!(op, BinaryOp::EqWildcard) {
        let one = ctx.alloc_vreg(SpillDesc::remat(1));
        block.push(MInst::LoadImm { dst: one, value: 1 });
        let value = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Select {
            dst: value,
            cond: has_mismatch,
            true_val: zero,
            false_val: one,
        });
        value
    } else {
        has_mismatch
    };
    let mask = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: mask,
        cond: has_mismatch,
        true_val: zero,
        false_val: has_unknown,
    });

    ctx.known_bits.insert(value, 1);
    ctx.set_wide_chunks(dst, vec![(value, 1)]);
    ctx.set_mask(dst, mask);
    ctx.wide_masks.insert(dst, vec![(mask, 1)]);
}

#[derive(Clone, Copy)]
enum ShiftDir {
    Left,
    Right,
    ArithRight,
}

/// Runtime multi-word shift via select chain + cross-chunk carry.
///
/// For each output chunk, a select chain picks the source chunk based on
/// `shift_amt >> 6` (word offset), then applies the intra-chunk bit shift
/// with carry from the adjacent chunk.
fn lower_wide_runtime_shift(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: &RegisterId,
    rhs: &RegisterId,
    n_chunks: usize,
    dir: ShiftDir,
    _is_sar: bool,
) {
    let src_chunks = ctx.get_wide_chunks(lhs, block);
    let n_src = src_chunks.len();
    let amount_vreg = ctx.reg_map.get(*rhs);

    let chunk_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::ShrImm {
        dst: chunk_shift,
        src: amount_vreg,
        imm: 6,
    });
    let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
    ctx.emit_and_imm(block, bit_shift, amount_vreg, 63);
    let sixty_four = ctx.alloc_vreg(SpillDesc::remat(64));
    block.push(MInst::LoadImm {
        dst: sixty_four,
        value: 64,
    });
    let inv_bit_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Sub {
        dst: inv_bit_shift,
        lhs: sixty_four,
        rhs: bit_shift,
    });
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let has_bit_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: has_bit_shift,
        lhs: bit_shift,
        rhs: zero,
        kind: CmpKind::Ne,
    });

    // Fill value: 0 for SHL/SHR, sign-extension for SAR
    let fill = if matches!(dir, ShiftDir::ArithRight) {
        let msb = src_chunks[n_src - 1].0;
        let sf = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::SarImm {
            dst: sf,
            src: msb,
            imm: 63,
        });
        sf
    } else {
        zero
    };

    let mut dst_chunks = Vec::with_capacity(n_chunks);
    for i in 0..n_chunks {
        // Select the "main" source chunk via word_offset.
        // For SHL: src_index = i - word_offset → select where j + word_offset == i
        // For SHR/SAR: src_index = i + word_offset → select where j == i + word_offset
        let main_chunk = {
            let mut val = fill;
            for j in (0..n_src).rev() {
                // Compute the effective index this source chunk maps to
                let j_vreg = ctx.alloc_vreg(SpillDesc::remat(j as u64));
                block.push(MInst::LoadImm {
                    dst: j_vreg,
                    value: j as u64,
                });
                let eff_idx = ctx.alloc_vreg(SpillDesc::transient());
                match dir {
                    ShiftDir::Left => {
                        // src[j] goes to dst[j + word_offset]
                        block.push(MInst::Add {
                            dst: eff_idx,
                            lhs: j_vreg,
                            rhs: chunk_shift,
                        });
                    }
                    ShiftDir::Right | ShiftDir::ArithRight => {
                        // src[j + word_offset] goes to dst[j], i.e., src[j] goes to dst[j - word_offset]
                        // Check: j >= word_offset, then eff = j - word_offset
                        // Simpler: for dst[i], source is src[i + word_offset]
                        // So we select j if j == i + word_offset
                        block.push(MInst::Sub {
                            dst: eff_idx,
                            lhs: j_vreg,
                            rhs: chunk_shift,
                        });
                    }
                }
                let i_vreg = ctx.alloc_vreg(SpillDesc::remat(i as u64));
                block.push(MInst::LoadImm {
                    dst: i_vreg,
                    value: i as u64,
                });
                let is_match = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: is_match,
                    lhs: eff_idx,
                    rhs: i_vreg,
                    kind: CmpKind::Eq,
                });
                let selected = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: selected,
                    cond: is_match,
                    true_val: src_chunks[j].0,
                    false_val: val,
                });
                val = selected;
            }
            val
        };

        // Select the "carry" source chunk (adjacent in shift direction)
        let carry_chunk = {
            let mut val = fill;
            for j in (0..n_src).rev() {
                let j_vreg = ctx.alloc_vreg(SpillDesc::remat(j as u64));
                block.push(MInst::LoadImm {
                    dst: j_vreg,
                    value: j as u64,
                });
                let eff_idx = ctx.alloc_vreg(SpillDesc::transient());
                let carry_i = match dir {
                    ShiftDir::Left => {
                        // carry comes from chunk below: i-1
                        if i == 0 { usize::MAX } else { i - 1 }
                    }
                    ShiftDir::Right | ShiftDir::ArithRight => {
                        // carry comes from chunk above: i+1
                        i + 1
                    }
                };
                match dir {
                    ShiftDir::Left => {
                        block.push(MInst::Add {
                            dst: eff_idx,
                            lhs: j_vreg,
                            rhs: chunk_shift,
                        });
                    }
                    ShiftDir::Right | ShiftDir::ArithRight => {
                        block.push(MInst::Sub {
                            dst: eff_idx,
                            lhs: j_vreg,
                            rhs: chunk_shift,
                        });
                    }
                }
                let ci_vreg = ctx.alloc_vreg(SpillDesc::remat(carry_i as u64));
                block.push(MInst::LoadImm {
                    dst: ci_vreg,
                    value: carry_i as u64,
                });
                let is_match = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: is_match,
                    lhs: eff_idx,
                    rhs: ci_vreg,
                    kind: CmpKind::Eq,
                });
                let selected = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: selected,
                    cond: is_match,
                    true_val: src_chunks[j].0,
                    false_val: val,
                });
                val = selected;
            }
            val
        };

        // Apply intra-chunk shift: result = (main_chunk SHIFT bit_shift) | (carry_chunk INVSHIFT inv_bit_shift)
        // (debug removed)
        let bit_shift_copy = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_mov(block, bit_shift_copy, bit_shift);
        let inv_copy = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_mov(block, inv_copy, inv_bit_shift);

        let main_shifted = ctx.alloc_vreg(SpillDesc::transient());
        let carry_shifted = ctx.alloc_vreg(SpillDesc::transient());

        match dir {
            ShiftDir::Left => {
                block.push(MInst::Shl {
                    dst: main_shifted,
                    lhs: main_chunk,
                    rhs: bit_shift_copy,
                });
                block.push(MInst::Shr {
                    dst: carry_shifted,
                    lhs: carry_chunk,
                    rhs: inv_copy,
                });
            }
            ShiftDir::Right => {
                block.push(MInst::Shr {
                    dst: main_shifted,
                    lhs: main_chunk,
                    rhs: bit_shift_copy,
                });
                block.push(MInst::Shl {
                    dst: carry_shifted,
                    lhs: carry_chunk,
                    rhs: inv_copy,
                });
            }
            ShiftDir::ArithRight => {
                if i == n_chunks - 1 {
                    block.push(MInst::Sar {
                        dst: main_shifted,
                        lhs: main_chunk,
                        rhs: bit_shift_copy,
                    });
                } else {
                    block.push(MInst::Shr {
                        dst: main_shifted,
                        lhs: main_chunk,
                        rhs: bit_shift_copy,
                    });
                }
                block.push(MInst::Shl {
                    dst: carry_shifted,
                    lhs: carry_chunk,
                    rhs: inv_copy,
                });
            }
        }

        // Combine: if has_bit_shift then (main | carry) else main
        let combined = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: combined,
            lhs: main_shifted,
            rhs: carry_shifted,
        });
        let result = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Select {
            dst: result,
            cond: has_bit_shift,
            true_val: combined,
            false_val: main_chunk,
        });

        dst_chunks.push((result, 64));
    }
    ctx.set_wide_chunks(dst, dst_chunks);
}

/// Reduce a wide value to a boolean (any chunk non-zero → 1, else 0).
fn wide_reduce_or(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    chunks: &[(VReg, usize)],
    n_chunks: usize,
) -> VReg {
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let mut acc = zero;
    for i in 0..n_chunks {
        let c = chunks.get(i).map(|c| c.0).unwrap_or(zero);
        let next = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: next,
            lhs: acc,
            rhs: c,
        });
        acc = next;
    }
    // acc != 0 → 1
    let result = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: result,
        lhs: acc,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    result
}

/// Mask a source word to its logical SIR width before a bit-count operation.
///
/// Loads normally zero-extend narrow values, but keeping the mask here makes
/// the count operations correct for every producer, including values that
/// reached ISel through a wide-to-narrow path.
fn mask_bit_count_word(ctx: &mut ISelContext, block: &mut MBlock, src: VReg, width: usize) -> VReg {
    if width >= 64 {
        src
    } else {
        let masked = ctx.alloc_vreg(SpillDesc::transient());
        ctx.emit_and_imm(block, masked, src, mask_for_width(width));
        masked
    }
}

fn bit_count_imm(ctx: &mut ISelContext, block: &mut MBlock, value: u64) -> VReg {
    let reg = ctx.alloc_vreg(SpillDesc::remat(value));
    block.push(MInst::LoadImm { dst: reg, value });
    reg
}

fn bit_count_nonzero(ctx: &mut ISelContext, block: &mut MBlock, src: VReg) -> VReg {
    let nonzero = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::CmpImm {
        dst: nonzero,
        lhs: src,
        imm: 0,
        kind: CmpKind::Ne,
    });
    nonzero
}

/// Return `(src != 0, base - bsr(src))`.  ORing bit 0 makes BSR defined for
/// zero without changing the highest set bit of any non-zero source.
fn clz_word_candidate(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    src: VReg,
    base: u64,
) -> (VReg, VReg) {
    let safe_src = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::OrImm {
        dst: safe_src,
        src,
        imm: 1,
    });
    let highest = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Bsr {
        dst: highest,
        src: safe_src,
    });
    let base = bit_count_imm(ctx, block, base);
    let candidate = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Sub {
        dst: candidate,
        lhs: base,
        rhs: highest,
    });
    (bit_count_nonzero(ctx, block, src), candidate)
}

/// Return `(src != 0, offset + ctz(src))`.  `x ^ (x - 1)` is non-zero for
/// every x and its highest set bit is ctz(x) whenever x itself is non-zero.
fn ctz_word_candidate(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    src: VReg,
    offset: u64,
) -> (VReg, VReg) {
    let decremented = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::SubImm {
        dst: decremented,
        src,
        imm: 1,
    });
    let low_span = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Xor {
        dst: low_span,
        lhs: src,
        rhs: decremented,
    });
    let local = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Bsr {
        dst: local,
        src: low_span,
    });
    let candidate = if offset == 0 {
        local
    } else {
        let offset = bit_count_imm(ctx, block, offset);
        let candidate = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Add {
            dst: candidate,
            lhs: offset,
            rhs: local,
        });
        candidate
    };
    (bit_count_nonzero(ctx, block, src), candidate)
}

/// Lower a bit-count operation whose source fits in one machine word.
fn lower_narrow_bit_count(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: VReg,
    op: &UnaryOp,
    src: VReg,
    src_width: usize,
) {
    if src_width == 0 {
        block.push(MInst::LoadImm { dst, value: 0 });
        return;
    }

    let src = mask_bit_count_word(ctx, block, src, src_width);
    let (nonzero, candidate) = match op {
        UnaryOp::PopCount => {
            block.push(MInst::Popcnt { dst, src });
            return;
        }
        UnaryOp::CountLeadingZeros => clz_word_candidate(ctx, block, src, (src_width - 1) as u64),
        UnaryOp::CountTrailingZeros => ctz_word_candidate(ctx, block, src, 0),
        _ => return,
    };
    let width = bit_count_imm(ctx, block, src_width as u64);
    block.push(MInst::Select {
        dst,
        cond: nonzero,
        true_val: candidate,
        false_val: width,
    });
}

/// Return chunk `index`, masked to the part that belongs to the logical source.
fn bit_count_chunk(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    chunks: &[(VReg, usize)],
    src_width: usize,
    index: usize,
) -> VReg {
    let chunk = ctx.wide_chunk_or_zero(chunks, index, block);
    let chunk_width = (src_width - index * 64).min(64);
    mask_bit_count_word(ctx, block, chunk, chunk_width)
}

/// Lower a bit-count operation over an arbitrary-width source.  The result of
/// all three operations is at most `src_width`, so its canonical SIR result
/// always fits in one native word even when the source spans many chunks.
fn lower_wide_bit_count(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    op: &UnaryOp,
    src: RegisterId,
) {
    let d_width = ctx.sir_width(&dst);
    let src_width = ctx.sir_width(&src);
    let chunks = ctx.get_wide_chunks(&src, block);
    let n_src = ISelContext::num_chunks(src_width);

    let result = match op {
        UnaryOp::PopCount => {
            let mut total = None;
            for index in 0..n_src {
                let chunk = bit_count_chunk(ctx, block, &chunks, src_width, index);
                let count = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Popcnt {
                    dst: count,
                    src: chunk,
                });
                total = Some(if let Some(total) = total {
                    let next = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: next,
                        lhs: total,
                        rhs: count,
                    });
                    next
                } else {
                    count
                });
            }
            total.unwrap_or_else(|| bit_count_imm(ctx, block, 0))
        }
        UnaryOp::CountLeadingZeros => {
            let mut count = bit_count_imm(ctx, block, src_width as u64);

            // Visiting chunks from least to most significant lets each
            // non-zero chunk overwrite the previous candidate; the last one
            // is therefore the highest non-zero chunk.
            for index in 0..n_src {
                let chunk = bit_count_chunk(ctx, block, &chunks, src_width, index);
                let base_value = src_width - 1 - index * 64;
                let (nonzero, candidate) = clz_word_candidate(ctx, block, chunk, base_value as u64);
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: next,
                    cond: nonzero,
                    true_val: candidate,
                    false_val: count,
                });
                count = next;
            }
            count
        }
        UnaryOp::CountTrailingZeros => {
            let mut count = bit_count_imm(ctx, block, src_width as u64);

            // Visiting chunks from most to least significant lets each
            // non-zero chunk overwrite the previous candidate; the last one
            // is therefore the lowest non-zero chunk.
            for index in (0..n_src).rev() {
                let chunk = bit_count_chunk(ctx, block, &chunks, src_width, index);
                let (nonzero, candidate) =
                    ctz_word_candidate(ctx, block, chunk, (index * 64) as u64);
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: next,
                    cond: nonzero,
                    true_val: candidate,
                    false_val: count,
                });
                count = next;
            }
            count
        }
        _ => return,
    };

    ctx.known_bits
        .insert(result, op.result_width(src_width).min(d_width));
    let n_dst = ISelContext::num_chunks(d_width).max(1);
    let mut dst_chunks = Vec::with_capacity(n_dst);
    dst_chunks.push((result, d_width.min(64)));
    for index in 1..n_dst {
        let zero = ctx.alloc_vreg(SpillDesc::remat(0));
        block.push(MInst::LoadImm {
            dst: zero,
            value: 0,
        });
        dst_chunks.push((zero, (d_width - index * 64).min(64)));
    }
    ctx.set_wide_chunks(dst, dst_chunks);
}

/// Lower a unary operation on wide (>64-bit) values.
fn lower_wide_unary(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    op: &UnaryOp,
    src: RegisterId,
) {
    let d_width = ctx.sir_width(&dst);
    let src_width = ctx.sir_width(&src);
    let n_chunks = ISelContext::num_chunks(d_width.max(src_width));

    match op {
        UnaryOp::BitNot => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            for i in 0..n_chunks {
                let s = src_chunks.get(i).map(|c| c.0).unwrap_or_else(|| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                });
                let d = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::BitNot { dst: d, src: s });
                dst_chunks.push((d, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }
        UnaryOp::Ident | UnaryOp::ToTwoState => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            // Zero-pad to n_chunks if source has fewer chunks (narrow→wide cast)
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            for i in 0..n_chunks {
                if i < src_chunks.len() {
                    dst_chunks.push(src_chunks[i]);
                } else {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    dst_chunks.push((z, 64));
                }
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }
        // Wide negation: two's complement = ~x + 1
        UnaryOp::Minus => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            // First invert all bits
            let mut inv_chunks = Vec::with_capacity(n_chunks);
            for i in 0..n_chunks {
                let s = ctx.wide_chunk_or_zero(&src_chunks, i, block);
                let d = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::BitNot { dst: d, src: s });
                inv_chunks.push((d, 64usize));
            }
            // Then add 1 (wide add with constant 1)
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            let mut carry: Option<VReg> = None;
            for (i, &(l, _)) in inv_chunks.iter().enumerate() {
                let r = if i == 0 {
                    let one = ctx.alloc_vreg(SpillDesc::remat(1));
                    block.push(MInst::LoadImm { dst: one, value: 1 });
                    one
                } else {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                };
                let s = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Add {
                    dst: s,
                    lhs: l,
                    rhs: r,
                });
                if let Some(cin) = carry {
                    let s2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Add {
                        dst: s2,
                        lhs: s,
                        rhs: cin,
                    });
                    let c1 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c1,
                        lhs: s,
                        rhs: l,
                        kind: CmpKind::LtU,
                    });
                    let c2 = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: c2,
                        lhs: s2,
                        rhs: s,
                        kind: CmpKind::LtU,
                    });
                    let cout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Or {
                        dst: cout,
                        lhs: c1,
                        rhs: c2,
                    });
                    carry = Some(cout);
                    dst_chunks.push((s2, 64));
                } else {
                    let cout = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: cout,
                        lhs: s,
                        rhs: l,
                        kind: CmpKind::LtU,
                    });
                    carry = Some(cout);
                    dst_chunks.push((s, 64));
                }
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide logical not: result = (value == 0) ? 1 : 0
        UnaryOp::LogicNot => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            let is_nonzero = wide_reduce_or(ctx, block, &src_chunks, n_chunks);
            // LogicNot: invert the boolean
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let result = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: result,
                lhs: is_nonzero,
                rhs: zero,
                kind: CmpKind::Eq,
            });

            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((result, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide reduction OR: result = (any bit set?) → 1
        UnaryOp::Or => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            let result = wide_reduce_or(ctx, block, &src_chunks, n_chunks);
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((result, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide reduction AND: result = (all bits set?) → 1
        UnaryOp::And => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            let all_ones = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
            block.push(MInst::LoadImm {
                dst: all_ones,
                value: u64::MAX,
            });
            let mut acc = all_ones;
            for i in 0..n_chunks {
                let c = ctx.wide_chunk_or_zero(&src_chunks, i, block);
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::And {
                    dst: next,
                    lhs: acc,
                    rhs: c,
                });
                acc = next;
            }
            let result = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: result,
                lhs: acc,
                rhs: all_ones,
                kind: CmpKind::Eq,
            });
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((result, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        // Wide reduction XOR: result = parity of all bits
        UnaryOp::Xor => {
            let src_chunks = ctx.get_wide_chunks(&src, block);
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let mut acc = zero;
            for i in 0..n_chunks {
                let c = ctx.wide_chunk_or_zero(&src_chunks, i, block);
                let next = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Xor {
                    dst: next,
                    lhs: acc,
                    rhs: c,
                });
                acc = next;
            }
            // Now acc has XOR of all chunks. Need popcount parity (odd # of 1-bits → 1)
            // Fold 64-bit value to 1 bit by cascading XOR
            let mut val = acc;
            for shift in [32u8, 16, 8, 4, 2, 1] {
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: shifted,
                    src: val,
                    imm: shift,
                });
                let folded = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Xor {
                    dst: folded,
                    lhs: val,
                    rhs: shifted,
                });
                val = folded;
            }
            let result = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, result, val, 1);
            let mut dst_chunks = Vec::with_capacity(n_chunks);
            dst_chunks.push((result, 64));
            for _ in 1..n_chunks {
                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm { dst: z, value: 0 });
                dst_chunks.push((z, 64));
            }
            ctx.set_wide_chunks(dst, dst_chunks);
        }

        UnaryOp::PopCount | UnaryOp::CountLeadingZeros | UnaryOp::CountTrailingZeros => {
            lower_wide_bit_count(ctx, block, dst, op, src);
        }
    }

    // Sync narrow results to scalar reg_map
    if d_width <= 64 {
        if let Some(chunks) = ctx.wide_regs.get(&dst) {
            let chunk0 = chunks[0].0;
            let scalar = ctx.reg_map.get(dst);
            if chunk0 != scalar {
                ctx.emit_alias_mov(block, scalar, chunk0);
            }
        }
    }
}

/// Extract a ≤64-bit value from a wide (>64-bit) register by right-shifting.
fn lower_wide_extract(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    wide_src: RegisterId,
    shift_amount: RegisterId,
) {
    let dst_vreg = ctx.reg_map.get(dst);
    let d_width = ctx.sir_width(&dst);
    let src_chunks = ctx.get_wide_chunks(&wide_src, block);
    let n_src = src_chunks.len();

    if let Some(&amount) = ctx.consts.get(&shift_amount) {
        // Constant extraction: directly pick the right chunk and shift
        let ci = (amount / 64) as usize;
        let is = (amount % 64) as u8;

        let main_vreg = if ci < n_src {
            src_chunks[ci].0
        } else {
            let z = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            z
        };

        if is == 0 {
            if d_width < 64 {
                let mask = mask_for_width(d_width);
                ctx.emit_and_imm(block, dst_vreg, main_vreg, mask);
            } else {
                ctx.emit_mov(block, dst_vreg, main_vreg);
            }
        } else {
            let shifted = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShrImm {
                dst: shifted,
                src: main_vreg,
                imm: is,
            });

            // Carry from next chunk
            if (ci + 1) < n_src {
                let next_vreg = src_chunks[ci + 1].0;
                let carry = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShlImm {
                    dst: carry,
                    src: next_vreg,
                    imm: 64 - is,
                });
                let combined = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: combined,
                    lhs: shifted,
                    rhs: carry,
                });
                if d_width < 64 {
                    let mask = mask_for_width(d_width);
                    ctx.emit_and_imm(block, dst_vreg, combined, mask);
                } else {
                    ctx.emit_mov(block, dst_vreg, combined);
                }
            } else if d_width < 64 {
                let mask = mask_for_width(d_width);
                ctx.emit_and_imm(block, dst_vreg, shifted, mask);
            } else {
                ctx.emit_mov(block, dst_vreg, shifted);
            }
        }
    } else {
        // Wide Shr with non-constant amount is handled by lower_wide_binary's
        // runtime shift path. This code is only reachable if the narrow Binary
        // handler's Shr detects lhs_width > 64, which is pre-empted by the wide
        // dispatch at the top of the Binary handler.
        unreachable!(
            "wide extract with non-constant shift: should be handled by lower_wide_binary"
        );
    }
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
                eprintln!(
                    "[isel-trace] terminator branch cond r{} -> {}",
                    cond.0, cond_vreg
                );
            }
            block.push(MInst::Branch {
                cond: cond_vreg,
                true_bb: BlockId(true_block.0.0 as u32),
                false_bb: BlockId(false_block.0.0 as u32),
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

fn lower_dynamic_wide_load_chunks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    base_off: i32,
    byte_off: VReg,
    offset_vreg: VReg,
    offset_reg: RegisterId,
    width_bits: usize,
) -> Vec<(VReg, usize)> {
    let n_chunks = ISelContext::num_chunks(width_bits);
    let mut chunks = Vec::with_capacity(n_chunks);

    if low_zero_bits_reg(ctx, offset_reg) >= 3 {
        let mut remaining = width_bits;
        let mut bit_pos = 0usize;
        while remaining > 0 {
            let chunk_bits = remaining.min(64);
            let chunk = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::LoadIndexed {
                dst: chunk,
                base: BaseReg::SimState,
                offset: base_off + (bit_pos / 8) as i32,
                index: byte_off,
                size: ISelContext::op_size_for_width(chunk_bits),
            });
            chunks.push((chunk, chunk_bits));
            bit_pos += chunk_bits;
            remaining -= chunk_bits;
        }
        return chunks;
    }

    let bit_shift = ctx.alloc_vreg(SpillDesc::transient());
    ctx.emit_and_imm(block, bit_shift, offset_vreg, 7);

    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let sixty_four = ctx.alloc_vreg(SpillDesc::remat(64));
    block.push(MInst::LoadImm {
        dst: sixty_four,
        value: 64,
    });
    let inv_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Sub {
        dst: inv_shift,
        lhs: sixty_four,
        rhs: bit_shift,
    });
    let inv_shift_mod = ctx.alloc_vreg(SpillDesc::transient());
    ctx.emit_and_imm(block, inv_shift_mod, inv_shift, 63);
    let has_shift = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: has_shift,
        lhs: bit_shift,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    ctx.known_bits.insert(has_shift, 1);

    let mut remaining = width_bits;
    let mut bit_pos = 0usize;
    while remaining > 0 {
        let chunk_bits = remaining.min(64);
        let byte_delta = (bit_pos / 8) as i32;
        let lo = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::LoadIndexed {
            dst: lo,
            base: BaseReg::SimState,
            offset: base_off + byte_delta,
            index: byte_off,
            size: OpSize::S64,
        });
        let lo_shifted = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Shr {
            dst: lo_shifted,
            lhs: lo,
            rhs: bit_shift,
        });

        let hi = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::LoadIndexed {
            dst: hi,
            base: BaseReg::SimState,
            offset: base_off + byte_delta + 8,
            index: byte_off,
            size: OpSize::S8,
        });
        let hi_shifted_raw = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Shl {
            dst: hi_shifted_raw,
            lhs: hi,
            rhs: inv_shift_mod,
        });
        let hi_shifted = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Select {
            dst: hi_shifted,
            cond: has_shift,
            true_val: hi_shifted_raw,
            false_val: zero,
        });

        let combined = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: combined,
            lhs: lo_shifted,
            rhs: hi_shifted,
        });
        let chunk = if chunk_bits < 64 {
            let masked = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked, combined, mask_for_width(chunk_bits));
            masked
        } else {
            combined
        };
        chunks.push((chunk, chunk_bits));

        bit_pos += chunk_bits;
        remaining -= chunk_bits;
    }

    chunks
}

// ────────────────────────────────────────────────────────────────
// 4-state mask computation
// ────────────────────────────────────────────────────────────────

fn lower_static_wide_load_chunks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    base_off: i32,
    bit_offset: usize,
    width_bits: usize,
) -> Vec<(VReg, usize)> {
    let n_chunks = ISelContext::num_chunks(width_bits).max(1);
    let mut chunks = Vec::with_capacity(n_chunks);
    let intra = bit_offset % 8;
    let first_byte = base_off + (bit_offset / 8) as i32;

    for index in 0..n_chunks {
        let chunk_bits = width_bits.saturating_sub(index * 64).min(64);
        let byte_off = first_byte + (index * 8) as i32;
        let needed_bits = chunk_bits + intra;
        let combined = if needed_bits <= 64 {
            let raw = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Load {
                dst: raw,
                base: BaseReg::SimState,
                offset: byte_off,
                size: ISelContext::op_size_for_width(needed_bits),
            });
            if intra == 0 {
                raw
            } else {
                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::ShrImm {
                    dst: shifted,
                    src: raw,
                    imm: intra as u8,
                });
                shifted
            }
        } else {
            let low = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Load {
                dst: low,
                base: BaseReg::SimState,
                offset: byte_off,
                size: OpSize::S64,
            });
            let shifted_low = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShrImm {
                dst: shifted_low,
                src: low,
                imm: intra as u8,
            });
            let high = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Load {
                dst: high,
                base: BaseReg::SimState,
                offset: byte_off + 8,
                size: OpSize::S8,
            });
            let shifted_high = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShlImm {
                dst: shifted_high,
                src: high,
                imm: (64 - intra) as u8,
            });
            let combined = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: combined,
                lhs: shifted_low,
                rhs: shifted_high,
            });
            combined
        };
        let value = if chunk_bits < 64 {
            let masked = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked, combined, mask_for_width(chunk_bits));
            masked
        } else {
            combined
        };
        chunks.push((value, chunk_bits));
    }
    chunks
}

/// Compute result mask for a binary operation.
///
/// Mask formulas (from IEEE 1800 / Cranelift backend):
/// - AND: m = (lm & rm) | (lm & rv) | (rm & lv)  ... but dominant-0 cancels X
///   Simplified: any X bit where the other operand's corresponding bit is not
///   definite-0 propagates as X. If the other bit is definite-0, AND = 0 regardless.
///   Formula: res_m = (lm | rm) & ~(~lv & ~lm) & ~(~rv & ~rm)
///   equivalently: res_m = (lm & rm) | (lm & rv) | (rm & lv)
/// - OR:  dual of AND — dominant-1 cancels X
///   res_m = (lm & rm) | (lm & ~rv) | (rm & ~lv)
/// - XOR: res_m = lm | rm
/// - Shift: if shift amount has X → all-X; else shift mask normally
/// - Arithmetic (Add/Sub/Mul/Div/Rem): conservative — any X → all-X
/// - Comparison: any X → result X (1-bit mask)
/// - LogicAnd: dominant-false (v|m==0) → mask=0; else if any X → mask=all-X
/// - LogicOr: dominant-true (v&~m!=0) → mask=0; else if any X → mask=all-X
fn lower_binary_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    op: &BinaryOp,
    lv: VReg,
    rv: VReg,
    lm: VReg,
    rm: VReg,
    d_width: usize,
) -> VReg {
    match op {
        BinaryOp::And => {
            // res_m = (lm & rm) | (lm & rv) | (rm & lv)
            let t1 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t1,
                lhs: lm,
                rhs: rm,
            });
            let t2 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t2,
                lhs: lm,
                rhs: rv,
            });
            let t3 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t3,
                lhs: rm,
                rhs: lv,
            });
            let t4 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: t4,
                lhs: t1,
                rhs: t2,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: res,
                lhs: t4,
                rhs: t3,
            });
            if d_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, res, mask_for_width(d_width));
                masked
            } else {
                res
            }
        }
        BinaryOp::Or => {
            // res_m = (lm & rm) | (lm & ~rv) | (rm & ~lv)
            let t1 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t1,
                lhs: lm,
                rhs: rm,
            });
            let not_rv = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_rv,
                src: rv,
            });
            let t2 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t2,
                lhs: lm,
                rhs: not_rv,
            });
            let not_lv = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_lv,
                src: lv,
            });
            let t3 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: t3,
                lhs: rm,
                rhs: not_lv,
            });
            let t4 = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: t4,
                lhs: t1,
                rhs: t2,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: res,
                lhs: t4,
                rhs: t3,
            });
            if d_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, res, mask_for_width(d_width));
                masked
            } else {
                res
            }
        }
        BinaryOp::Xor => {
            // res_m = lm | rm
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: res,
                lhs: lm,
                rhs: rm,
            });
            if d_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, res, mask_for_width(d_width));
                masked
            } else {
                res
            }
        }
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
            // If shift amount has X → all-X; else shift mask by same amount
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_x,
                lhs: rm,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            // Shift mask by value
            let shifted_m = ctx.alloc_vreg(SpillDesc::transient());
            match op {
                BinaryOp::Shl => block.push(MInst::Shl {
                    dst: shifted_m,
                    lhs: lm,
                    rhs: rv,
                }),
                BinaryOp::Shr => block.push(MInst::Shr {
                    dst: shifted_m,
                    lhs: lm,
                    rhs: rv,
                }),
                BinaryOp::Sar => block.push(MInst::Sar {
                    dst: shifted_m,
                    lhs: lm,
                    rhs: rv,
                }),
                _ => unreachable!(),
            }
            let all_x = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
            block.push(MInst::LoadImm {
                dst: all_x,
                value: u64::MAX,
            });
            let raw = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: raw,
                cond: has_x,
                true_val: all_x,
                false_val: shifted_m,
            });
            if d_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, raw, mask_for_width(d_width));
                masked
            } else {
                raw
            }
        }
        BinaryOp::LogicAnd => {
            // Dominant false: if either operand is definite false (v|m == 0) → mask=0
            // else if any X → mask = all-X
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let l_vm = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: l_vm,
                lhs: lv,
                rhs: lm,
            });
            let r_vm = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: r_vm,
                lhs: rv,
                rhs: rm,
            });
            let l_def_false = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: l_def_false,
                lhs: l_vm,
                rhs: zero,
                kind: CmpKind::Eq,
            });
            let r_def_false = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: r_def_false,
                lhs: r_vm,
                rhs: zero,
                kind: CmpKind::Eq,
            });
            let either_false = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: either_false,
                lhs: l_def_false,
                rhs: r_def_false,
            });
            // any X?
            let l_has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: l_has_x,
                lhs: lm,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let r_has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: r_has_x,
                lhs: rm,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let any_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: any_x,
                lhs: l_has_x,
                rhs: r_has_x,
            });
            let all_ones = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(d_width)));
            block.push(MInst::LoadImm {
                dst: all_ones,
                value: mask_for_width(d_width),
            });
            let conservative = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: conservative,
                cond: any_x,
                true_val: all_ones,
                false_val: zero,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: res,
                cond: either_false,
                true_val: zero,
                false_val: conservative,
            });
            res
        }
        BinaryOp::LogicOr => {
            // Dominant true: if either operand is definite true → mask=0
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let not_lm = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_lm,
                src: lm,
            });
            let l_def_v = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: l_def_v,
                lhs: lv,
                rhs: not_lm,
            });
            let not_rm = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_rm,
                src: rm,
            });
            let r_def_v = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: r_def_v,
                lhs: rv,
                rhs: not_rm,
            });
            let l_def_true = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: l_def_true,
                lhs: l_def_v,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let r_def_true = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: r_def_true,
                lhs: r_def_v,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let either_true = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: either_true,
                lhs: l_def_true,
                rhs: r_def_true,
            });
            let l_has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: l_has_x,
                lhs: lm,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let r_has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: r_has_x,
                lhs: rm,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let any_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: any_x,
                lhs: l_has_x,
                rhs: r_has_x,
            });
            let all_ones = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(d_width)));
            block.push(MInst::LoadImm {
                dst: all_ones,
                value: mask_for_width(d_width),
            });
            let conservative = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: conservative,
                cond: any_x,
                true_val: all_ones,
                false_val: zero,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: res,
                cond: either_true,
                true_val: zero,
                false_val: conservative,
            });
            res
        }
        BinaryOp::EqWildcard | BinaryOp::NeWildcard => {
            // Wildcard mask is handled inline in the Binary handler;
            // this arm should never be reached.
            unreachable!("wildcard mask is computed inline, not via lower_binary_mask")
        }
        _ => {
            // Conservative: any X in either operand → all-X result
            // Covers: Add, Sub, Mul, Div, Rem, comparisons (Eq/Ne/Lt/Le/Gt/Ge)
            conservative_mask(ctx, block, lm, rm, d_width)
        }
    }
}

/// Compute result mask for a unary operation.
fn lower_unary_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    op: &UnaryOp,
    src_v: VReg,
    src_m: VReg,
    d_width: usize,
    src_width: usize,
) -> VReg {
    match op {
        UnaryOp::ToTwoState => {
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            zero
        }
        UnaryOp::Ident => {
            // Mask passes through but must be zero-extended for widening.
            // When src is narrower than dst, upper bits should be 0 (definite 0).
            let effective_width = src_width.min(d_width);
            if effective_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, src_m, mask_for_width(effective_width));
                masked
            } else {
                src_m
            }
        }
        UnaryOp::BitNot => {
            // ~X = X, mask passes through
            if d_width < 64 {
                let masked = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, masked, src_m, mask_for_width(d_width));
                masked
            } else {
                src_m
            }
        }
        UnaryOp::Minus
        | UnaryOp::PopCount
        | UnaryOp::CountLeadingZeros
        | UnaryOp::CountTrailingZeros => {
            // Conservative: any X → all-X
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_x,
                lhs: src_m,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let all_ones = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(d_width)));
            block.push(MInst::LoadImm {
                dst: all_ones,
                value: mask_for_width(d_width),
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: res,
                cond: has_x,
                true_val: all_ones,
                false_val: zero,
            });
            res
        }
        UnaryOp::And => {
            // Reduction AND: dominant-0. If any definite 0 → result definite (mask=0).
            // definite_zeros = ~src_v & ~src_m (bits that are definitely 0)
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let not_v = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_v,
                src: src_v,
            });
            let not_m = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_m,
                src: src_m,
            });
            let def_zeros = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: def_zeros,
                lhs: not_v,
                rhs: not_m,
            });
            // Mask to src_width
            let def_zeros_masked = if src_width < 64 {
                let m = ctx.alloc_vreg(SpillDesc::transient());
                ctx.emit_and_imm(block, m, def_zeros, mask_for_width(src_width));
                m
            } else {
                def_zeros
            };
            let has_def_zero = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_def_zero,
                lhs: def_zeros_masked,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_x,
                lhs: src_m,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            // If definite 0 → mask=0; else if X → mask=1; else mask=0
            let x_mask = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: x_mask,
                cond: has_x,
                true_val: has_x,
                false_val: zero,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: res,
                cond: has_def_zero,
                true_val: zero,
                false_val: x_mask,
            });
            res
        }
        UnaryOp::LogicNot | UnaryOp::Or => {
            // Reduction OR: dominant-1. If any definite 1 → result definite (mask=0).
            // definite_ones = src_v & ~src_m (bits that are definitely 1)
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let not_m = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::BitNot {
                dst: not_m,
                src: src_m,
            });
            let def_ones = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::And {
                dst: def_ones,
                lhs: src_v,
                rhs: not_m,
            });
            let has_def_one = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_def_one,
                lhs: def_ones,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_x,
                lhs: src_m,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            let x_mask = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: x_mask,
                cond: has_x,
                true_val: has_x,
                false_val: zero,
            });
            let res = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: res,
                cond: has_def_one,
                true_val: zero,
                false_val: x_mask,
            });
            res
        }
        UnaryOp::Xor => {
            // Reduction XOR: any X → result X
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: has_x,
                lhs: src_m,
                rhs: zero,
                kind: CmpKind::Ne,
            });
            has_x
        }
    }
}

/// Conservative mask: if either operand has any X bits, result is all-X.
fn conservative_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    lm: VReg,
    rm: VReg,
    d_width: usize,
) -> VReg {
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    let l_has_x = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: l_has_x,
        lhs: lm,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    let r_has_x = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: r_has_x,
        lhs: rm,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    let any_x = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Or {
        dst: any_x,
        lhs: l_has_x,
        rhs: r_has_x,
    });
    let all_ones = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(d_width)));
    block.push(MInst::LoadImm {
        dst: all_ones,
        value: mask_for_width(d_width),
    });
    let res = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Select {
        dst: res,
        cond: any_x,
        true_val: all_ones,
        false_val: zero,
    });
    res
}

/// Normalize wide 4-state value: operations produce X (v=1,m=1), never Z.
/// For each chunk, computes `v_chunk |= m_chunk`.
fn normalize_wide_value(ctx: &mut ISelContext, block: &mut MBlock, dst: RegisterId) {
    let mask_chunks: Vec<VReg> = if let Some(mc) = ctx.wide_masks.get(&dst).cloned() {
        mc.iter().map(|c| c.0).collect()
    } else {
        return;
    };

    if let Some(val_chunks) = ctx.wide_regs.get(&dst).cloned() {
        let mut new_chunks = Vec::with_capacity(val_chunks.len());
        for (i, &(vc, width)) in val_chunks.iter().enumerate() {
            if let Some(&mc) = mask_chunks.get(i) {
                let normed = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: normed,
                    lhs: vc,
                    rhs: mc,
                });
                new_chunks.push((normed, width));
            } else {
                new_chunks.push((vc, width));
            }
        }
        ctx.set_wide_chunks(dst, new_chunks);
    }
}

/// Collapse unknown source bits to zero for an explicit four-state to
/// two-state conversion, then clear every destination mask chunk.
fn lower_wide_to_two_state(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    src: RegisterId,
    d_width: usize,
    src_width: usize,
) {
    let source_masks =
        get_wide_mask_chunks(ctx, block, &src, ISelContext::num_chunks(src_width).max(1));
    let values = ctx.get_wide_chunks(&dst, block);
    let n_dst = ISelContext::num_chunks(d_width).max(1);
    let mut cleared_chunks = Vec::with_capacity(n_dst);

    for index in 0..n_dst {
        let value = ctx.wide_chunk_or_zero(&values, index, block);
        let source_mask = source_masks.get(index).copied().unwrap_or_else(|| {
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            zero
        });
        let defined = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::BitNot {
            dst: defined,
            src: source_mask,
        });
        let cleared = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::And {
            dst: cleared,
            lhs: value,
            rhs: defined,
        });
        let chunk_width = (d_width - index * 64).min(64);
        cleared_chunks.push((cleared, chunk_width));
    }

    ctx.set_wide_chunks(dst, cleared_chunks);
}

// ────────────────────────────────────────────────────────────────
// Wide (>64-bit) 4-state mask computation
// ────────────────────────────────────────────────────────────────

/// Helper: get wide mask chunks for a register, or create zero chunks.
fn get_wide_mask_chunks(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    reg: &RegisterId,
    n_chunks: usize,
) -> Vec<VReg> {
    if let Some(mchunks) = ctx.wide_masks.get(reg).cloned() {
        let mut result: Vec<VReg> = mchunks.iter().map(|c| c.0).collect();
        while result.len() < n_chunks {
            let z = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            result.push(z);
        }
        result
    } else {
        // Use scalar mask if available
        let scalar_m = ctx.mask_map.map.get(reg.0).copied().flatten();
        let mut result = Vec::with_capacity(n_chunks);
        if let Some(m) = scalar_m {
            result.push(m);
        } else {
            let z = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            result.push(z);
        }
        for _ in 1..n_chunks {
            let z = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm { dst: z, value: 0 });
            result.push(z);
        }
        result
    }
}

/// Slice the four-state mask in exactly the same bit positions as the value.
/// Keeping this separate from value selection avoids the static-load shortcut
/// silently leaving a destination mask undefined.
fn lower_slice_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    src: RegisterId,
    bit_offset: usize,
    width: usize,
) {
    let src_width = ctx.sir_width(&src);
    let n_src = ISelContext::num_chunks(src_width).max(1);
    let src_chunks = get_wide_mask_chunks(ctx, block, &src, n_src);
    let n_dst = ISelContext::num_chunks(width).max(1);
    let mut dst_chunks = Vec::with_capacity(n_dst);

    let chunk_or_zero = |ctx: &mut ISelContext, block: &mut MBlock, index: usize| -> VReg {
        src_chunks.get(index).copied().unwrap_or_else(|| {
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            zero
        })
    };

    for dst_index in 0..n_dst {
        let absolute_bit = bit_offset + dst_index * 64;
        let src_index = absolute_bit / 64;
        let intra_bit = absolute_bit % 64;
        let low = chunk_or_zero(ctx, block, src_index);
        let combined = if intra_bit == 0 {
            low
        } else {
            let shifted_low = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShrImm {
                dst: shifted_low,
                src: low,
                imm: intra_bit as u8,
            });
            let high = chunk_or_zero(ctx, block, src_index + 1);
            let shifted_high = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::ShlImm {
                dst: shifted_high,
                src: high,
                imm: (64 - intra_bit) as u8,
            });
            let combined = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: combined,
                lhs: shifted_low,
                rhs: shifted_high,
            });
            combined
        };

        let chunk_width = (width - dst_index * 64).min(64);
        let masked = if chunk_width < 64 {
            let masked = ctx.alloc_vreg(SpillDesc::transient());
            ctx.emit_and_imm(block, masked, combined, mask_for_width(chunk_width));
            masked
        } else {
            combined
        };
        dst_chunks.push((masked, chunk_width));
    }

    ctx.set_mask(dst, dst_chunks[0].0);
    if width > 64 {
        ctx.wide_masks.insert(dst, dst_chunks);
    }
}

/// Check if any chunk has X bits (OR-reduce all mask chunks).
fn any_chunk_has_x(ctx: &mut ISelContext, block: &mut MBlock, mask_chunks: &[VReg]) -> VReg {
    let zero = ctx.alloc_vreg(SpillDesc::remat(0));
    block.push(MInst::LoadImm {
        dst: zero,
        value: 0,
    });
    // OR all mask chunks together
    let mut combined = mask_chunks[0];
    for &mc in &mask_chunks[1..] {
        let t = ctx.alloc_vreg(SpillDesc::transient());
        block.push(MInst::Or {
            dst: t,
            lhs: combined,
            rhs: mc,
        });
        combined = t;
    }
    let has_x = ctx.alloc_vreg(SpillDesc::transient());
    block.push(MInst::Cmp {
        dst: has_x,
        lhs: combined,
        rhs: zero,
        kind: CmpKind::Ne,
    });
    has_x
}

/// Compute wide mask for binary operations.
fn lower_wide_binary_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    lhs: RegisterId,
    op: &BinaryOp,
    rhs: RegisterId,
    d_width: usize,
) {
    // Four-state wide wildcard comparisons lower value and mask together so
    // a definite mismatch can dominate an unknown LHS bit across chunks.
    if matches!(op, BinaryOp::EqWildcard | BinaryOp::NeWildcard) {
        return;
    }

    let n_chunks =
        ISelContext::num_chunks(d_width.max(ctx.sir_width(&lhs)).max(ctx.sir_width(&rhs)));
    let lm_chunks = get_wide_mask_chunks(ctx, block, &lhs, n_chunks);
    let rm_chunks = get_wide_mask_chunks(ctx, block, &rhs, n_chunks);

    match op {
        BinaryOp::And | BinaryOp::Or | BinaryOp::Xor => {
            // Per-chunk mask computation
            let lv_chunks = ctx.get_wide_chunks(&lhs, block);
            let rv_chunks = ctx.get_wide_chunks(&rhs, block);
            let n_dst = ISelContext::num_chunks(d_width);
            let mut dst_m_chunks = Vec::with_capacity(n_dst);

            for i in 0..n_dst {
                let lm = lm_chunks.get(i).copied().unwrap_or_else(|| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                });
                let rm = rm_chunks.get(i).copied().unwrap_or_else(|| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                });
                let lv = ctx.wide_chunk_or_zero(&lv_chunks, i, block);
                let rv = ctx.wide_chunk_or_zero(&rv_chunks, i, block);
                let chunk_w = if i == n_dst - 1 {
                    let r = d_width % 64;
                    if r == 0 { 64 } else { r }
                } else {
                    64
                };
                let m = lower_binary_mask(ctx, block, op, lv, rv, lm, rm, chunk_w);
                dst_m_chunks.push((m, 64));
            }
            // Set scalar mask from chunk[0]
            ctx.set_mask(dst, dst_m_chunks[0].0);
            ctx.wide_masks.insert(dst, dst_m_chunks);
        }
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => {
            // If shift amount has X → all-X. Otherwise, shift mask same way as value.
            // Check shift amount mask (rhs is scalar, so rm_chunks[0] is the mask)
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let shift_has_x = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Cmp {
                dst: shift_has_x,
                lhs: rm_chunks[0],
                rhs: zero,
                kind: CmpKind::Ne,
            });

            let n_dst = ISelContext::num_chunks(d_width);
            // Get the result value chunks (already computed by lower_wide_binary)
            // The mask should follow the same pattern as the value.
            // For constant shifts, shift mask chunks directly.
            if let Some(&amount) = ctx.consts.get(&rhs) {
                let cs = (amount / 64) as usize;
                let is = (amount % 64) as u8;
                let mut dst_m_chunks = Vec::with_capacity(n_dst);

                match op {
                    BinaryOp::Shl => {
                        for i in 0..n_dst {
                            if i < cs {
                                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                block.push(MInst::LoadImm { dst: z, value: 0 });
                                dst_m_chunks.push((z, 64));
                            } else {
                                let src_i = i - cs;
                                let cur = lm_chunks.get(src_i).copied().unwrap_or_else(|| {
                                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                    block.push(MInst::LoadImm { dst: z, value: 0 });
                                    z
                                });
                                if is == 0 {
                                    dst_m_chunks.push((cur, 64));
                                } else {
                                    let shifted = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::ShlImm {
                                        dst: shifted,
                                        src: cur,
                                        imm: is,
                                    });
                                    let prev = if src_i > 0 {
                                        lm_chunks.get(src_i - 1).copied().unwrap_or_else(|| {
                                            let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                            block.push(MInst::LoadImm { dst: z, value: 0 });
                                            z
                                        })
                                    } else {
                                        let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                        block.push(MInst::LoadImm { dst: z, value: 0 });
                                        z
                                    };
                                    let carry = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::ShrImm {
                                        dst: carry,
                                        src: prev,
                                        imm: 64 - is,
                                    });
                                    let merged = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::Or {
                                        dst: merged,
                                        lhs: shifted,
                                        rhs: carry,
                                    });
                                    dst_m_chunks.push((merged, 64));
                                }
                            }
                        }
                    }
                    BinaryOp::Shr | BinaryOp::Sar => {
                        for i in 0..n_dst {
                            let src_i = i + cs;
                            let cur = lm_chunks.get(src_i).copied().unwrap_or_else(|| {
                                let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                block.push(MInst::LoadImm { dst: z, value: 0 });
                                z
                            });
                            if is == 0 {
                                dst_m_chunks.push((cur, 64));
                            } else {
                                let shifted = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShrImm {
                                    dst: shifted,
                                    src: cur,
                                    imm: is,
                                });
                                let next = lm_chunks.get(src_i + 1).copied().unwrap_or_else(|| {
                                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                    block.push(MInst::LoadImm { dst: z, value: 0 });
                                    z
                                });
                                let carry = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::ShlImm {
                                    dst: carry,
                                    src: next,
                                    imm: 64 - is,
                                });
                                let merged = ctx.alloc_vreg(SpillDesc::transient());
                                block.push(MInst::Or {
                                    dst: merged,
                                    lhs: shifted,
                                    rhs: carry,
                                });
                                dst_m_chunks.push((merged, 64));
                            }
                        }

                        // SAR: if the sign bit is X, sign-extension produces X in upper bits.
                        // Check if bit (lhs_width-1) in the mask is set.
                        if matches!(op, BinaryOp::Sar) {
                            let lhs_w = ctx.sir_width(&lhs);
                            let sign_chunk = (lhs_w - 1) / 64;
                            let sign_bit = (lhs_w - 1) % 64;
                            let sign_mask_chunk =
                                lm_chunks.get(sign_chunk).copied().unwrap_or_else(|| {
                                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                                    block.push(MInst::LoadImm { dst: z, value: 0 });
                                    z
                                });
                            // Extract sign bit from mask
                            let sign_x = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::ShrImm {
                                dst: sign_x,
                                src: sign_mask_chunk,
                                imm: sign_bit as u8,
                            });
                            let one = ctx.alloc_vreg(SpillDesc::remat(1));
                            block.push(MInst::LoadImm { dst: one, value: 1 });
                            let sign_x_bit = ctx.alloc_vreg(SpillDesc::transient());
                            block.push(MInst::And {
                                dst: sign_x_bit,
                                lhs: sign_x,
                                rhs: one,
                            });
                            let sign_is_x = ctx.alloc_vreg(SpillDesc::transient());
                            let z_cmp = ctx.alloc_vreg(SpillDesc::remat(0));
                            block.push(MInst::LoadImm {
                                dst: z_cmp,
                                value: 0,
                            });
                            block.push(MInst::Cmp {
                                dst: sign_is_x,
                                lhs: sign_x_bit,
                                rhs: z_cmp,
                                kind: CmpKind::Ne,
                            });

                            // For chunks above the shifted sign position, OR with all-X if sign is X.
                            // The sign bit after shift is at position (lhs_width - 1 - shift_amount).
                            // All bits above this position in the result are sign-extended.
                            let effective_sign_pos =
                                lhs_w.saturating_sub(1).saturating_sub(amount as usize);
                            let all_ones = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
                            block.push(MInst::LoadImm {
                                dst: all_ones,
                                value: u64::MAX,
                            });
                            for (i, chunk) in dst_m_chunks.iter_mut().enumerate() {
                                let chunk_start = i * 64;
                                if chunk_start >= effective_sign_pos {
                                    // Entire chunk is above sign — all X if sign is X
                                    let new_m = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::Select {
                                        dst: new_m,
                                        cond: sign_is_x,
                                        true_val: all_ones,
                                        false_val: chunk.0,
                                    });
                                    chunk.0 = new_m;
                                } else if chunk_start + 64 > effective_sign_pos {
                                    // Partial: bits above effective_sign_pos in this chunk
                                    let bit_in_chunk = effective_sign_pos - chunk_start;
                                    let upper_mask_val = u64::MAX << bit_in_chunk;
                                    let upper_mask =
                                        ctx.alloc_vreg(SpillDesc::remat(upper_mask_val));
                                    block.push(MInst::LoadImm {
                                        dst: upper_mask,
                                        value: upper_mask_val,
                                    });
                                    let x_fill = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::Select {
                                        dst: x_fill,
                                        cond: sign_is_x,
                                        true_val: upper_mask,
                                        false_val: z_cmp,
                                    });
                                    let new_m = ctx.alloc_vreg(SpillDesc::transient());
                                    block.push(MInst::Or {
                                        dst: new_m,
                                        lhs: chunk.0,
                                        rhs: x_fill,
                                    });
                                    chunk.0 = new_m;
                                }
                            }
                        }
                    }
                    _ => unreachable!(),
                }

                // Apply X-in-shift-amount override
                let all_x = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
                block.push(MInst::LoadImm {
                    dst: all_x,
                    value: u64::MAX,
                });
                let mut final_chunks = Vec::with_capacity(n_dst);
                for (i, (m, w)) in dst_m_chunks.into_iter().enumerate() {
                    let chunk_w = if i == n_dst - 1 {
                        let r = d_width % 64;
                        if r == 0 { 64 } else { r }
                    } else {
                        64
                    };
                    let x_val = if chunk_w < 64 {
                        let v = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(chunk_w)));
                        block.push(MInst::LoadImm {
                            dst: v,
                            value: mask_for_width(chunk_w),
                        });
                        v
                    } else {
                        all_x
                    };
                    let res = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: res,
                        cond: shift_has_x,
                        true_val: x_val,
                        false_val: m,
                    });
                    final_chunks.push((res, w));
                }
                ctx.set_mask(dst, final_chunks[0].0);
                ctx.wide_masks.insert(dst, final_chunks);
            } else {
                // Runtime shift: apply the same shift to mask chunks.
                // Temporarily inject mask chunks as wide_regs for a pseudo register,
                // call lower_wide_runtime_shift, then extract result.
                // Use pseudo RegisterIds just past the existing range
                let next_id = ctx.reg_map.map.len();
                let mask_pseudo_lhs = RegisterId(next_id);
                let mask_pseudo_dst = RegisterId(next_id + 1);
                let mask_chunks_wide: Vec<(VReg, usize)> =
                    lm_chunks.iter().map(|&v| (v, 64usize)).collect();
                ctx.wide_regs.insert(mask_pseudo_lhs, mask_chunks_wide);
                // pseudo_dst needs a scalar VReg in reg_map
                let pd_vreg = ctx.alloc_vreg(SpillDesc::transient());
                ctx.reg_map.map.resize(next_id + 2, None);
                ctx.reg_map.map[mask_pseudo_dst.0] = Some(pd_vreg);

                let dir = match op {
                    BinaryOp::Shl => ShiftDir::Left,
                    _ => ShiftDir::Right,
                };
                lower_wide_runtime_shift(
                    ctx,
                    block,
                    mask_pseudo_dst,
                    &mask_pseudo_lhs,
                    &rhs,
                    n_chunks,
                    dir,
                    false,
                );

                // Extract shifted mask chunks
                let shifted_mask_chunks =
                    ctx.wide_regs.remove(&mask_pseudo_dst).unwrap_or_default();
                ctx.wide_regs.remove(&mask_pseudo_lhs);

                // Apply shift-has-X override
                let all_x_v = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
                block.push(MInst::LoadImm {
                    dst: all_x_v,
                    value: u64::MAX,
                });
                let mut final_m_chunks = Vec::with_capacity(n_dst);
                for i in 0..n_dst {
                    let chunk_w = if i == n_dst - 1 {
                        let r = d_width % 64;
                        if r == 0 { 64 } else { r }
                    } else {
                        64
                    };
                    let x_val = if chunk_w < 64 {
                        let v = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(chunk_w)));
                        block.push(MInst::LoadImm {
                            dst: v,
                            value: mask_for_width(chunk_w),
                        });
                        v
                    } else {
                        all_x_v
                    };
                    let shifted_m = shifted_mask_chunks.get(i).map(|c| c.0).unwrap_or_else(|| {
                        let z = ctx.alloc_vreg(SpillDesc::remat(0));
                        block.push(MInst::LoadImm { dst: z, value: 0 });
                        z
                    });
                    let res = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: res,
                        cond: shift_has_x,
                        true_val: x_val,
                        false_val: shifted_m,
                    });
                    final_m_chunks.push((res, 64));
                }
                ctx.set_mask(dst, final_m_chunks[0].0);
                ctx.wide_masks.insert(dst, final_m_chunks);
            }
        }
        BinaryOp::LogicAnd | BinaryOp::LogicOr => {
            let (lhs_is_true, lhs_is_unknown) = lower_mux_condition_state(ctx, block, lhs);
            let (rhs_is_true, rhs_is_unknown) = lower_mux_condition_state(ctx, block, rhs);
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });

            let dominant = if matches!(op, BinaryOp::LogicAnd) {
                let lhs_not_false = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: lhs_not_false,
                    lhs: lhs_is_true,
                    rhs: lhs_is_unknown,
                });
                let lhs_is_false = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: lhs_is_false,
                    lhs: lhs_not_false,
                    rhs: zero,
                    kind: CmpKind::Eq,
                });
                let rhs_not_false = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: rhs_not_false,
                    lhs: rhs_is_true,
                    rhs: rhs_is_unknown,
                });
                let rhs_is_false = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Cmp {
                    dst: rhs_is_false,
                    lhs: rhs_not_false,
                    rhs: zero,
                    kind: CmpKind::Eq,
                });
                let either_is_false = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: either_is_false,
                    lhs: lhs_is_false,
                    rhs: rhs_is_false,
                });
                either_is_false
            } else {
                let either_is_true = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Or {
                    dst: either_is_true,
                    lhs: lhs_is_true,
                    rhs: rhs_is_true,
                });
                either_is_true
            };
            let either_is_unknown = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Or {
                dst: either_is_unknown,
                lhs: lhs_is_unknown,
                rhs: rhs_is_unknown,
            });
            let result_mask = ctx.alloc_vreg(SpillDesc::transient());
            block.push(MInst::Select {
                dst: result_mask,
                cond: dominant,
                true_val: zero,
                false_val: either_is_unknown,
            });
            ctx.set_mask(dst, result_mask);
            ctx.wide_masks.insert(dst, vec![(result_mask, d_width)]);
        }
        _ => {
            // Conservative: any X in any chunk of either operand → all-X result
            let all_masks: Vec<VReg> = lm_chunks.iter().chain(rm_chunks.iter()).copied().collect();
            let has_x = any_chunk_has_x(ctx, block, &all_masks);

            let n_dst = ISelContext::num_chunks(d_width);
            if n_dst == 0 {
                // 1-bit result (comparison)
                ctx.set_mask(dst, has_x);
                return;
            }
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });

            if d_width <= 64 {
                // Narrow result from wide operands (comparisons)
                let all_ones = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(d_width)));
                block.push(MInst::LoadImm {
                    dst: all_ones,
                    value: mask_for_width(d_width),
                });
                let res = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: res,
                    cond: has_x,
                    true_val: all_ones,
                    false_val: zero,
                });
                ctx.set_mask(dst, res);
            } else {
                let mut dst_m_chunks = Vec::with_capacity(n_dst);
                let all_ones = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
                block.push(MInst::LoadImm {
                    dst: all_ones,
                    value: u64::MAX,
                });
                for i in 0..n_dst {
                    let chunk_m = ctx.alloc_vreg(SpillDesc::transient());
                    let chunk_w = if i == n_dst - 1 {
                        let r = d_width % 64;
                        if r == 0 { 64 } else { r }
                    } else {
                        64
                    };
                    if chunk_w < 64 {
                        let mask_val = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(chunk_w)));
                        block.push(MInst::LoadImm {
                            dst: mask_val,
                            value: mask_for_width(chunk_w),
                        });
                        block.push(MInst::Select {
                            dst: chunk_m,
                            cond: has_x,
                            true_val: mask_val,
                            false_val: zero,
                        });
                    } else {
                        block.push(MInst::Select {
                            dst: chunk_m,
                            cond: has_x,
                            true_val: all_ones,
                            false_val: zero,
                        });
                    }
                    dst_m_chunks.push((chunk_m, 64));
                }
                ctx.set_mask(dst, dst_m_chunks[0].0);
                ctx.wide_masks.insert(dst, dst_m_chunks);
            }
        }
    }
}

/// Compute wide mask for unary operations.
fn lower_wide_unary_mask(
    ctx: &mut ISelContext,
    block: &mut MBlock,
    dst: RegisterId,
    op: &UnaryOp,
    src: RegisterId,
    d_width: usize,
    src_width: usize,
) {
    let n_src = ISelContext::num_chunks(src_width);
    let sm_chunks = get_wide_mask_chunks(ctx, block, &src, n_src);

    match op {
        UnaryOp::ToTwoState => {
            let n_dst = ISelContext::num_chunks(d_width);
            let mut dst_m_chunks = Vec::with_capacity(n_dst);
            for index in 0..n_dst {
                let zero = ctx.alloc_vreg(SpillDesc::remat(0));
                block.push(MInst::LoadImm {
                    dst: zero,
                    value: 0,
                });
                let chunk_width = (d_width - index * 64).min(64);
                dst_m_chunks.push((zero, chunk_width));
            }
            ctx.set_mask(dst, dst_m_chunks[0].0);
            ctx.wide_masks.insert(dst, dst_m_chunks);
        }
        UnaryOp::Ident | UnaryOp::BitNot => {
            // Mask passes through (per-chunk)
            let n_dst = ISelContext::num_chunks(d_width);
            let mut dst_m_chunks = Vec::with_capacity(n_dst);
            for i in 0..n_dst {
                let m = sm_chunks.get(i).copied().unwrap_or_else(|| {
                    let z = ctx.alloc_vreg(SpillDesc::remat(0));
                    block.push(MInst::LoadImm { dst: z, value: 0 });
                    z
                });
                dst_m_chunks.push((m, 64));
            }
            ctx.set_mask(dst, dst_m_chunks[0].0);
            ctx.wide_masks.insert(dst, dst_m_chunks);
        }
        UnaryOp::Minus
        | UnaryOp::PopCount
        | UnaryOp::CountLeadingZeros
        | UnaryOp::CountTrailingZeros => {
            // Conservative: any X → all-X
            let has_x = any_chunk_has_x(ctx, block, &sm_chunks);
            let n_dst = ISelContext::num_chunks(d_width);
            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });

            if d_width <= 64 {
                let all_ones = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(d_width)));
                block.push(MInst::LoadImm {
                    dst: all_ones,
                    value: mask_for_width(d_width),
                });
                let res = ctx.alloc_vreg(SpillDesc::transient());
                block.push(MInst::Select {
                    dst: res,
                    cond: has_x,
                    true_val: all_ones,
                    false_val: zero,
                });
                ctx.set_mask(dst, res);
                ctx.wide_masks.insert(dst, vec![(res, d_width)]);
            } else {
                let all_ones = ctx.alloc_vreg(SpillDesc::remat(u64::MAX));
                block.push(MInst::LoadImm {
                    dst: all_ones,
                    value: u64::MAX,
                });
                let mut dst_m_chunks = Vec::with_capacity(n_dst);
                for i in 0..n_dst {
                    let chunk_m = ctx.alloc_vreg(SpillDesc::transient());
                    let chunk_w = if i == n_dst - 1 {
                        let r = d_width % 64;
                        if r == 0 { 64 } else { r }
                    } else {
                        64
                    };
                    if chunk_w < 64 {
                        let mask_val = ctx.alloc_vreg(SpillDesc::remat(mask_for_width(chunk_w)));
                        block.push(MInst::LoadImm {
                            dst: mask_val,
                            value: mask_for_width(chunk_w),
                        });
                        block.push(MInst::Select {
                            dst: chunk_m,
                            cond: has_x,
                            true_val: mask_val,
                            false_val: zero,
                        });
                    } else {
                        block.push(MInst::Select {
                            dst: chunk_m,
                            cond: has_x,
                            true_val: all_ones,
                            false_val: zero,
                        });
                    }
                    dst_m_chunks.push((chunk_m, 64));
                }
                ctx.set_mask(dst, dst_m_chunks[0].0);
                ctx.wide_masks.insert(dst, dst_m_chunks);
            }
        }
        UnaryOp::And | UnaryOp::LogicNot | UnaryOp::Or | UnaryOp::Xor => {
            // Reduction ops: result is 1-bit
            // AND: dominant-0 across all chunks
            // OR: dominant-1 across all chunks
            // XOR: any X → result X
            let sv_chunks: Vec<VReg> = if let Some(chunks) = ctx.wide_regs.get(&src).cloned() {
                chunks.iter().map(|c| c.0).collect()
            } else {
                vec![ctx.reg_map.get(src)]
            };

            let zero = ctx.alloc_vreg(SpillDesc::remat(0));
            block.push(MInst::LoadImm {
                dst: zero,
                value: 0,
            });
            let has_x = any_chunk_has_x(ctx, block, &sm_chunks);

            match op {
                UnaryOp::And => {
                    // Check for definite-0 across all chunks: ~v & ~m
                    let mut any_def_zero = zero;
                    for i in 0..n_src {
                        let not_v = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: not_v,
                            src: sv_chunks[i.min(sv_chunks.len() - 1)],
                        });
                        let not_m = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: not_m,
                            src: sm_chunks[i],
                        });
                        let def_z = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: def_z,
                            lhs: not_v,
                            rhs: not_m,
                        });
                        let combined = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: combined,
                            lhs: any_def_zero,
                            rhs: def_z,
                        });
                        any_def_zero = combined;
                    }
                    let has_def_zero = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: has_def_zero,
                        lhs: any_def_zero,
                        rhs: zero,
                        kind: CmpKind::Ne,
                    });
                    let x_mask = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: x_mask,
                        cond: has_x,
                        true_val: has_x,
                        false_val: zero,
                    });
                    let res = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: res,
                        cond: has_def_zero,
                        true_val: zero,
                        false_val: x_mask,
                    });
                    ctx.set_mask(dst, res);
                    ctx.wide_masks.insert(dst, vec![(res, d_width)]);
                }
                UnaryOp::LogicNot | UnaryOp::Or => {
                    // Check for definite-1 across all chunks: v & ~m
                    let mut any_def_one = zero;
                    for i in 0..n_src {
                        let not_m = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::BitNot {
                            dst: not_m,
                            src: sm_chunks[i],
                        });
                        let def_one = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::And {
                            dst: def_one,
                            lhs: sv_chunks[i.min(sv_chunks.len() - 1)],
                            rhs: not_m,
                        });
                        let combined = ctx.alloc_vreg(SpillDesc::transient());
                        block.push(MInst::Or {
                            dst: combined,
                            lhs: any_def_one,
                            rhs: def_one,
                        });
                        any_def_one = combined;
                    }
                    let has_def_one = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Cmp {
                        dst: has_def_one,
                        lhs: any_def_one,
                        rhs: zero,
                        kind: CmpKind::Ne,
                    });
                    let x_mask = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: x_mask,
                        cond: has_x,
                        true_val: has_x,
                        false_val: zero,
                    });
                    let res = ctx.alloc_vreg(SpillDesc::transient());
                    block.push(MInst::Select {
                        dst: res,
                        cond: has_def_one,
                        true_val: zero,
                        false_val: x_mask,
                    });
                    ctx.set_mask(dst, res);
                    ctx.wide_masks.insert(dst, vec![(res, d_width)]);
                }
                _ => {
                    // XOR: any X → result X
                    ctx.set_mask(dst, has_x);
                    ctx.wide_masks.insert(dst, vec![(has_x, d_width)]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::native::{emit, jit_mem::JitCode, mir_legalize, mir_opt, regalloc};
    use crate::ir::{AbsoluteAddr, BasicBlock, BlockId as SirBlockId, InstanceId, SIRValue};
    use num_bigint::BigUint;
    use veryl_analyzer::ir::VarId;

    fn execute_unaligned_64_bit_load(dynamic: bool) -> u64 {
        let input_var = VarId::default();
        let mut output_var = input_var;
        output_var.inc();
        let input_abs = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: input_var,
        };
        let output_abs = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: output_var,
        };
        let input_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, input_abs);
        let output_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs);
        let offset = RegisterId(0);
        let loaded = RegisterId(1);
        let mut instructions = Vec::new();
        let load_offset = if dynamic {
            instructions.push(SIRInstruction::Imm(offset, SIRValue::new(6u8)));
            SIROffset::Dynamic(offset)
        } else {
            SIROffset::Static(6)
        };
        instructions.push(SIRInstruction::Load(loaded, input_addr, load_offset, 64));
        instructions.push(SIRInstruction::Store(
            output_addr,
            SIROffset::Static(0),
            64,
            loaded,
            vec![],
            vec![],
        ));
        let eu = ExecutionUnit {
            entry_block_id: SirBlockId(0),
            blocks: [(
                SirBlockId(0),
                BasicBlock {
                    id: SirBlockId(0),
                    params: vec![],
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map: [
                (
                    offset,
                    RegisterType::Bit {
                        width: 7,
                        signed: false,
                    },
                ),
                (
                    loaded,
                    RegisterType::Bit {
                        width: 64,
                        signed: false,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        };
        eu.verify();

        let layout = MemoryLayout {
            four_state: false,
            offsets: [(input_abs, 0), (output_abs, 16)].into_iter().collect(),
            widths: [(input_abs, 72), (output_abs, 64)].into_iter().collect(),
            is_4states: [(input_abs, false), (output_abs, false)]
                .into_iter()
                .collect(),
            total_size: 24,
            working_offsets: HashMap::default(),
            working_base_offset: 24,
            sparse_offsets: HashMap::default(),
            sparse_base_offset: 24,
            sparse_layouts: HashMap::default(),
            merged_total_size: 24,
            triggered_bits_offset: 24,
            triggered_bits_total_size: 0,
            scratch_base_offset: 24,
            scratch_size: 0,
            runtime_event_capacity: 0,
            runtime_event_slot_size: 0,
            runtime_event_buffer_size: 0,
            runtime_event_site_layouts: vec![],
        };
        let mut function = lower_execution_unit(&eu, &layout, false);
        mir_legalize::legalize(&mut function);
        mir_opt::optimize(&mut function);
        let allocation = regalloc::run_regalloc(&mut function).unwrap();
        mir_opt::post_regalloc_peephole(&mut function);
        function.verify();
        let emitted = emit::emit(
            &function,
            &allocation.assignment,
            allocation.spill_frame_size,
        )
        .unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let expected = 0xfedc_ba98_8000_0004u64;
        let input = (BigUint::from(expected) << 6usize).to_bytes_le();
        let mut state = vec![0u8; 24];
        state[..input.len()].copy_from_slice(&input);
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        u64::from_le_bytes(state[16..24].try_into().unwrap())
    }

    #[test]
    fn static_unaligned_64_bit_load_preserves_crossing_bits() {
        assert_eq!(execute_unaligned_64_bit_load(false), 0xfedc_ba98_8000_0004);
    }

    #[test]
    fn dynamic_unaligned_64_bit_load_preserves_crossing_bits() {
        assert_eq!(execute_unaligned_64_bit_load(true), 0xfedc_ba98_8000_0004);
    }

    fn get_bits(bytes: &[u8], bit_offset: usize, width: usize) -> u64 {
        let mut value = 0u64;
        for bit in 0..width {
            let source = bit_offset + bit;
            value |= u64::from((bytes[source / 8] >> (source % 8)) & 1) << bit;
        }
        value
    }

    fn set_bits(bytes: &mut [u8], bit_offset: usize, width: usize, value: u64) {
        for bit in 0..width {
            let destination = bit_offset + bit;
            let mask = 1u8 << (destination % 8);
            if (value >> bit) & 1 != 0 {
                bytes[destination / 8] |= mask;
            } else {
                bytes[destination / 8] &= !mask;
            }
        }
    }

    fn verify_scalar_alignment_matrix(dynamic: bool, four_state: bool) {
        const SLOT: usize = 768;
        const CASE_STRIDE: usize = SLOT * 3;
        const DYNAMIC_OFFSETS: &[usize] = &[
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 13, 63, 64, 65, 309, 618, 927, 2163,
        ];
        let mut instructions = Vec::new();
        let mut register_map = HashMap::default();
        let mut offsets = HashMap::default();
        let mut widths = HashMap::default();
        let mut is_4states = HashMap::default();
        let mut cases = Vec::new();
        let mut next_reg = 0usize;
        let mut case_index = 0usize;

        for width in 1..=64usize {
            for &bit_offset in DYNAMIC_OFFSETS {
                let storage_width = bit_offset + width;
                let source_abs = AbsoluteAddr {
                    instance_id: InstanceId(0),
                    var_id: VarId::from_raw((case_index * 3) as u32),
                };
                let output_abs = AbsoluteAddr {
                    instance_id: InstanceId(0),
                    var_id: VarId::from_raw((case_index * 3 + 1) as u32),
                };
                let destination_abs = AbsoluteAddr {
                    instance_id: InstanceId(0),
                    var_id: VarId::from_raw((case_index * 3 + 2) as u32),
                };
                let base = case_index * CASE_STRIDE;
                offsets.insert(source_abs, base);
                offsets.insert(output_abs, base + SLOT);
                offsets.insert(destination_abs, base + SLOT * 2);
                widths.insert(source_abs, storage_width);
                widths.insert(output_abs, width);
                widths.insert(destination_abs, storage_width);
                is_4states.insert(source_abs, four_state);
                is_4states.insert(output_abs, four_state);
                is_4states.insert(destination_abs, four_state);

                let loaded = RegisterId(next_reg);
                next_reg += 1;
                register_map.insert(
                    loaded,
                    if four_state {
                        RegisterType::Logic { width }
                    } else {
                        RegisterType::Bit {
                            width,
                            signed: false,
                        }
                    },
                );
                let offset_operand = if dynamic {
                    let offset_reg = RegisterId(next_reg);
                    next_reg += 1;
                    register_map.insert(
                        offset_reg,
                        RegisterType::Bit {
                            width: 12,
                            signed: false,
                        },
                    );
                    instructions.push(SIRInstruction::Imm(
                        offset_reg,
                        SIRValue::new(bit_offset as u64),
                    ));
                    SIROffset::Dynamic(offset_reg)
                } else {
                    SIROffset::Static(bit_offset)
                };
                instructions.push(SIRInstruction::Load(
                    loaded,
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, source_abs),
                    offset_operand.clone(),
                    width,
                ));
                instructions.push(SIRInstruction::Store(
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs),
                    SIROffset::Static(0),
                    width,
                    loaded,
                    vec![],
                    vec![],
                ));
                instructions.push(SIRInstruction::Store(
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, destination_abs),
                    offset_operand,
                    width,
                    loaded,
                    vec![],
                    vec![],
                ));
                cases.push((base, width, bit_offset, storage_width));
                case_index += 1;
            }
        }

        let total_size = case_index * CASE_STRIDE;
        let eu = ExecutionUnit {
            entry_block_id: SirBlockId(0),
            blocks: [(
                SirBlockId(0),
                BasicBlock {
                    id: SirBlockId(0),
                    params: vec![],
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map,
        };
        eu.verify();
        let layout = MemoryLayout {
            four_state,
            offsets,
            widths,
            is_4states,
            total_size,
            working_offsets: HashMap::default(),
            working_base_offset: total_size,
            sparse_offsets: HashMap::default(),
            sparse_base_offset: total_size,
            sparse_layouts: HashMap::default(),
            merged_total_size: total_size,
            triggered_bits_offset: total_size,
            triggered_bits_total_size: 0,
            scratch_base_offset: total_size,
            scratch_size: 0,
            runtime_event_capacity: 0,
            runtime_event_slot_size: 0,
            runtime_event_buffer_size: 0,
            runtime_event_site_layouts: vec![],
        };
        let mut function = lower_execution_unit(&eu, &layout, four_state);
        mir_legalize::legalize(&mut function);
        mir_opt::optimize(&mut function);
        let allocation = regalloc::run_regalloc(&mut function).unwrap();
        mir_opt::post_regalloc_peephole(&mut function);
        function.verify();
        let emitted = emit::emit(
            &function,
            &allocation.assignment,
            allocation.spill_frame_size,
        )
        .unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();

        let mut state = vec![0u8; total_size];
        for (index, byte) in state.iter_mut().enumerate() {
            *byte = (index as u8).wrapping_mul(73).wrapping_add(0x5b);
        }
        let before = state.clone();
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        for (base, width, bit_offset, storage_width) in cases {
            let source_bytes = storage_width.div_ceil(8);
            let output_bytes = width.div_ceil(8);
            let expected = get_bits(&before[base..base + SLOT], bit_offset, width);
            assert_eq!(
                get_bits(&state[base + SLOT..base + SLOT * 2], 0, width),
                expected,
                "value load mismatch: dynamic={dynamic} four_state={four_state} width={width} bit_offset={bit_offset}"
            );
            let mut expected_destination = before[base + SLOT * 2..base + SLOT * 3].to_vec();
            set_bits(&mut expected_destination, bit_offset, width, expected);
            if four_state {
                let expected_mask =
                    get_bits(&before[base + source_bytes..base + SLOT], bit_offset, width);
                assert_eq!(
                    get_bits(
                        &state[base + SLOT + output_bytes..base + SLOT * 2],
                        0,
                        width,
                    ),
                    expected_mask,
                    "mask load mismatch: dynamic={dynamic} width={width} bit_offset={bit_offset}"
                );
                set_bits(
                    &mut expected_destination,
                    source_bytes * 8 + bit_offset,
                    width,
                    expected_mask,
                );
            }
            let destination = &state[base + SLOT * 2..base + SLOT * 3];
            for bit in 0..storage_width {
                assert_eq!(
                    get_bits(destination, bit, 1),
                    get_bits(&expected_destination, bit, 1),
                    "value store mismatch: dynamic={dynamic} four_state={four_state} width={width} bit_offset={bit_offset} bit={bit}"
                );
                if four_state {
                    assert_eq!(
                        get_bits(destination, source_bytes * 8 + bit, 1),
                        get_bits(&expected_destination, source_bytes * 8 + bit, 1),
                        "mask store mismatch: dynamic={dynamic} width={width} bit_offset={bit_offset} bit={bit}"
                    );
                }
            }
            let allocated_bytes = source_bytes * if four_state { 2 } else { 1 };
            assert_eq!(
                &destination[allocated_bytes..],
                &expected_destination[allocated_bytes..],
                "store clobbered adjacent storage: dynamic={dynamic} four_state={four_state} width={width} bit_offset={bit_offset}"
            );
        }
    }

    #[test]
    fn static_scalar_load_store_alignment_matrix() {
        verify_scalar_alignment_matrix(false, false);
    }

    #[test]
    fn dynamic_scalar_load_store_alignment_matrix() {
        verify_scalar_alignment_matrix(true, false);
    }

    #[test]
    fn static_four_state_scalar_load_store_alignment_matrix() {
        verify_scalar_alignment_matrix(false, true);
    }

    #[test]
    fn dynamic_four_state_scalar_load_store_alignment_matrix() {
        verify_scalar_alignment_matrix(true, true);
    }

    fn verify_wide_alignment_matrix(dynamic: bool) {
        const SLOT: usize = 384;
        const CASE_STRIDE: usize = SLOT * 3;
        const DYNAMIC_OFFSETS: &[usize] = &[
            0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 13, 63, 64, 65, 309, 618, 927, 2163,
        ];
        let tested_widths = [65usize, 72, 127, 128, 129, 255, 274, 309];
        let mut instructions = Vec::new();
        let mut register_map = HashMap::default();
        let mut offsets = HashMap::default();
        let mut widths = HashMap::default();
        let mut is_4states = HashMap::default();
        let mut cases = Vec::new();
        let mut next_reg = 0usize;
        let mut case_index = 0usize;

        for width in tested_widths {
            for &bit_offset in DYNAMIC_OFFSETS {
                let storage_width = bit_offset + width;
                let source_abs = AbsoluteAddr {
                    instance_id: InstanceId(0),
                    var_id: VarId::from_raw((case_index * 3) as u32),
                };
                let output_abs = AbsoluteAddr {
                    instance_id: InstanceId(0),
                    var_id: VarId::from_raw((case_index * 3 + 1) as u32),
                };
                let destination_abs = AbsoluteAddr {
                    instance_id: InstanceId(0),
                    var_id: VarId::from_raw((case_index * 3 + 2) as u32),
                };
                let base = case_index * CASE_STRIDE;
                offsets.insert(source_abs, base);
                offsets.insert(output_abs, base + SLOT);
                offsets.insert(destination_abs, base + SLOT * 2);
                widths.insert(source_abs, storage_width);
                widths.insert(output_abs, width);
                widths.insert(destination_abs, storage_width);
                is_4states.insert(source_abs, false);
                is_4states.insert(output_abs, false);
                is_4states.insert(destination_abs, false);

                let loaded = RegisterId(next_reg);
                next_reg += 1;
                register_map.insert(
                    loaded,
                    RegisterType::Bit {
                        width,
                        signed: false,
                    },
                );
                let offset_operand = if dynamic {
                    let offset_reg = RegisterId(next_reg);
                    next_reg += 1;
                    register_map.insert(
                        offset_reg,
                        RegisterType::Bit {
                            width: 12,
                            signed: false,
                        },
                    );
                    instructions.push(SIRInstruction::Imm(
                        offset_reg,
                        SIRValue::new(bit_offset as u64),
                    ));
                    SIROffset::Dynamic(offset_reg)
                } else {
                    SIROffset::Static(bit_offset)
                };
                instructions.push(SIRInstruction::Load(
                    loaded,
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, source_abs),
                    offset_operand.clone(),
                    width,
                ));
                instructions.push(SIRInstruction::Store(
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs),
                    SIROffset::Static(0),
                    width,
                    loaded,
                    vec![],
                    vec![],
                ));
                instructions.push(SIRInstruction::Store(
                    RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, destination_abs),
                    offset_operand,
                    width,
                    loaded,
                    vec![],
                    vec![],
                ));
                cases.push((base, width, bit_offset));
                case_index += 1;
            }
        }

        let total_size = case_index * CASE_STRIDE;
        let eu = ExecutionUnit {
            entry_block_id: SirBlockId(0),
            blocks: [(
                SirBlockId(0),
                BasicBlock {
                    id: SirBlockId(0),
                    params: vec![],
                    instructions,
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map,
        };
        eu.verify();
        let layout = MemoryLayout {
            four_state: false,
            offsets,
            widths,
            is_4states,
            total_size,
            working_offsets: HashMap::default(),
            working_base_offset: total_size,
            sparse_offsets: HashMap::default(),
            sparse_base_offset: total_size,
            sparse_layouts: HashMap::default(),
            merged_total_size: total_size,
            triggered_bits_offset: total_size,
            triggered_bits_total_size: 0,
            scratch_base_offset: total_size,
            scratch_size: 0,
            runtime_event_capacity: 0,
            runtime_event_slot_size: 0,
            runtime_event_buffer_size: 0,
            runtime_event_site_layouts: vec![],
        };
        let mut function = lower_execution_unit(&eu, &layout, false);
        mir_legalize::legalize(&mut function);
        mir_opt::optimize(&mut function);
        let allocation = regalloc::run_regalloc(&mut function).unwrap();
        mir_opt::post_regalloc_peephole(&mut function);
        function.verify();
        let emitted = emit::emit(
            &function,
            &allocation.assignment,
            allocation.spill_frame_size,
        )
        .unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        let mut state = vec![0u8; total_size];
        for (index, byte) in state.iter_mut().enumerate() {
            *byte = (index as u8).wrapping_mul(73).wrapping_add(0x5b);
        }
        let before = state.clone();
        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        for (base, width, bit_offset) in cases {
            let mut expected_destination = before[base + SLOT * 2..base + SLOT * 3].to_vec();
            for bit in 0..width {
                let source_bit = get_bits(&before[base..base + SLOT], bit_offset + bit, 1);
                set_bits(&mut expected_destination, bit_offset + bit, 1, source_bit);
                assert_eq!(
                    get_bits(&state[base + SLOT..base + SLOT * 2], bit, 1),
                    source_bit,
                    "wide load mismatch: dynamic={dynamic} width={width} bit_offset={bit_offset} bit={bit}"
                );
            }
            assert_eq!(
                &state[base + SLOT * 2..base + SLOT * 3],
                expected_destination.as_slice(),
                "wide store mismatch: dynamic={dynamic} width={width} bit_offset={bit_offset}"
            );
        }
    }

    #[test]
    fn static_wide_load_store_alignment_matrix() {
        verify_wide_alignment_matrix(false);
    }

    #[test]
    fn dynamic_wide_load_store_alignment_matrix() {
        verify_wide_alignment_matrix(true);
    }

    #[test]
    fn narrowed_wide_binary_store_does_not_write_source_width() {
        let source_var = VarId::default();
        let mut destination_var = source_var;
        destination_var.inc();
        let source_abs = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: source_var,
        };
        let destination_abs = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: destination_var,
        };
        let source_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, source_abs);
        let destination_addr =
            RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, destination_abs);

        let source = RegisterId(0);
        let shift_amount = RegisterId(1);
        let shifted = RegisterId(2);
        let width_mask = RegisterId(3);
        let narrowed = RegisterId(4);
        let bit_type = |width| RegisterType::Bit {
            width,
            signed: false,
        };
        let eu = ExecutionUnit {
            entry_block_id: SirBlockId(0),
            blocks: [(
                SirBlockId(0),
                BasicBlock {
                    id: SirBlockId(0),
                    params: vec![],
                    instructions: vec![
                        SIRInstruction::Load(source, source_addr, SIROffset::Static(0), 309),
                        SIRInstruction::Imm(shift_amount, SIRValue::new(35u8)),
                        SIRInstruction::Binary(shifted, source, BinaryOp::Shr, shift_amount),
                        SIRInstruction::Imm(
                            width_mask,
                            SIRValue::new((BigUint::from(1u8) << 274usize) - BigUint::from(1u8)),
                        ),
                        SIRInstruction::Binary(narrowed, shifted, BinaryOp::And, width_mask),
                        SIRInstruction::Store(
                            destination_addr,
                            SIROffset::Static(35),
                            274,
                            narrowed,
                            vec![],
                            vec![],
                        ),
                    ],
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map: [
                (source, bit_type(309)),
                (shift_amount, bit_type(8)),
                (shifted, bit_type(309)),
                (width_mask, bit_type(274)),
                (narrowed, bit_type(274)),
            ]
            .into_iter()
            .collect(),
        };
        eu.verify();

        let layout = MemoryLayout {
            four_state: false,
            offsets: [(source_abs, 0), (destination_abs, 64)]
                .into_iter()
                .collect(),
            widths: [(source_abs, 309), (destination_abs, 344)]
                .into_iter()
                .collect(),
            is_4states: [(source_abs, false), (destination_abs, false)]
                .into_iter()
                .collect(),
            total_size: 112,
            working_offsets: HashMap::default(),
            working_base_offset: 112,
            sparse_offsets: HashMap::default(),
            sparse_base_offset: 112,
            sparse_layouts: HashMap::default(),
            merged_total_size: 112,
            triggered_bits_offset: 112,
            triggered_bits_total_size: 0,
            scratch_base_offset: 112,
            scratch_size: 0,
            runtime_event_capacity: 0,
            runtime_event_slot_size: 0,
            runtime_event_buffer_size: 0,
            runtime_event_site_layouts: vec![],
        };
        let mut function = lower_execution_unit(&eu, &layout, false);
        mir_legalize::legalize(&mut function);
        mir_opt::optimize(&mut function);
        let allocation = regalloc::run_regalloc(&mut function).unwrap();
        mir_opt::post_regalloc_peephole(&mut function);
        function.verify();
        let emitted = emit::emit(
            &function,
            &allocation.assignment,
            allocation.spill_frame_size,
        )
        .unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();

        let mut state = vec![0xa5u8; 112];
        for (index, byte) in state[..39].iter_mut().enumerate() {
            *byte = (index as u8).wrapping_mul(73).wrapping_add(0x5b);
        }
        let before = state.clone();
        let mut expected = before[64..107].to_vec();
        for bit in 0..274 {
            let value = get_bits(&before[..39], bit + 35, 1);
            set_bits(&mut expected, bit + 35, 1, value);
        }

        assert_eq!(unsafe { jit.call(&mut state) }, 0);
        for bit in 0..344 {
            assert_eq!(
                get_bits(&state[64..107], bit, 1),
                get_bits(&expected, bit, 1),
                "destination bit {bit} differs"
            );
        }
        assert_eq!(&state[107..], &before[107..], "store exceeded its variable");
    }

    struct LookupFixture {
        eu: ExecutionUnit<RegionedAbsoluteAddr>,
        block_id: SirBlockId,
        roots: Vec<(RegisterId, usize)>,
        selector: RegisterId,
        alternate_selector: RegisterId,
        defaults: Vec<RegisterId>,
        key_defs: Vec<(RegisterId, usize)>,
        conditions: Vec<(RegisterId, usize, usize)>,
        mux_indices: Vec<Vec<usize>>,
    }

    struct LookupFixtureBuilder {
        next_reg: usize,
        register_map: HashMap<RegisterId, RegisterType>,
        constants: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    }

    impl LookupFixtureBuilder {
        fn new() -> Self {
            Self {
                next_reg: 0,
                register_map: HashMap::default(),
                constants: Vec::new(),
                instructions: Vec::new(),
            }
        }

        fn register(&mut self, width: usize) -> RegisterId {
            let reg = RegisterId(self.next_reg);
            self.next_reg += 1;
            self.register_map.insert(
                reg,
                RegisterType::Bit {
                    width,
                    signed: false,
                },
            );
            reg
        }

        fn constant(&mut self, width: usize, value: u64, mask: u64) -> (RegisterId, usize) {
            let reg = self.register(width);
            let idx = self.constants.len();
            self.constants.push(SIRInstruction::Imm(
                reg,
                SIRValue::new_four_state(value, mask),
            ));
            (reg, idx)
        }

        fn instruction(
            &mut self,
            width: usize,
            make: impl FnOnce(RegisterId) -> SIRInstruction<RegionedAbsoluteAddr>,
        ) -> (RegisterId, usize) {
            let reg = self.register(width);
            let idx = self.instructions.len();
            self.instructions.push(make(reg));
            (reg, idx)
        }
    }

    fn dense_lookup_fixture(root_count: usize) -> LookupFixture {
        let mut builder = LookupFixtureBuilder::new();
        let selector = builder.register(2);
        let alternate_selector = builder.register(2);
        let (zero, _) = builder.constant(1, 0, 0);
        let mut key_defs = Vec::new();
        for key in 0..4 {
            key_defs.push(builder.constant(2, key, 0));
        }
        let mut defaults = Vec::new();
        let mut value_regs = Vec::new();
        for root in 0..root_count {
            defaults.push(builder.constant(8, 0xe0 + root as u64, 0).0);
            let mut values = Vec::new();
            for key in 0..4 {
                // Key 3 deliberately carries a payload bit outside the
                // logical result width.  Table construction must truncate it
                // exactly like ordinary SIR immediate lowering.
                let value = if root == 0 && key == 3 {
                    0x100 + 13
                } else {
                    10 + root as u64 * 16 + key as u64
                };
                values.push(builder.constant(8, value, 0).0);
            }
            value_regs.push(values);
        }

        // Use a non-sorted stage order so the test observes that key/value
        // association, rather than mux position, defines the table entry.
        let stage_keys = [2usize, 0, 3, 1];
        let mut conditions = Vec::new();
        for (stage, &key) in stage_keys.iter().enumerate() {
            let key_reg = key_defs[key].0;
            let (compare, compare_idx) = if stage % 2 == 0 {
                builder.instruction(1, |dst| {
                    SIRInstruction::Binary(dst, selector, BinaryOp::EqWildcard, key_reg)
                })
            } else {
                // Exact equality is symmetric; exercise a constant LHS too.
                builder.instruction(1, |dst| {
                    SIRInstruction::Binary(dst, key_reg, BinaryOp::Eq, selector)
                })
            };
            let (condition, concat_idx) =
                builder.instruction(2, |dst| SIRInstruction::Concat(dst, vec![zero, compare]));
            conditions.push((condition, compare_idx, concat_idx));
        }

        let mut roots = Vec::new();
        let mut mux_indices = Vec::new();
        for root in 0..root_count {
            let mut previous = defaults[root];
            let mut indices = Vec::new();
            for (stage, &key) in stage_keys.iter().enumerate() {
                let condition = conditions[stage].0;
                let then_value = value_regs[root][key];
                let (next, idx) = builder.instruction(8, |dst| {
                    SIRInstruction::Mux(dst, condition, then_value, previous)
                });
                previous = next;
                indices.push(idx);
            }
            roots.push((previous, *indices.last().unwrap()));
            mux_indices.push(indices);
        }

        let constants_block = BasicBlock {
            id: SirBlockId(0),
            params: vec![],
            instructions: builder.constants,
            terminator: SIRTerminator::Jump(SirBlockId(1), vec![]),
        };
        let lookup_block = BasicBlock {
            id: SirBlockId(1),
            params: vec![],
            instructions: builder.instructions,
            terminator: SIRTerminator::Return,
        };
        LookupFixture {
            eu: ExecutionUnit {
                entry_block_id: SirBlockId(0),
                blocks: [
                    (SirBlockId(0), constants_block),
                    (SirBlockId(1), lookup_block),
                ]
                .into_iter()
                .collect(),
                register_map: builder.register_map,
            },
            block_id: SirBlockId(1),
            roots,
            selector,
            alternate_selector,
            defaults,
            key_defs,
            conditions,
            mux_indices,
        }
    }

    fn lookup_plans(fixture: &LookupFixture) -> DenseLookupPlans {
        let constants = collect_exact_sir_constants(&fixture.eu);
        let uses = collect_sir_use_sites(&fixture.eu);
        find_dense_lookup_plans(
            &fixture.eu.blocks[&fixture.block_id],
            &fixture.eu.register_map,
            &constants,
            &uses,
        )
    }

    #[test]
    fn recognizes_full_domain_dense_lookup_with_global_constants_and_zero_extended_conditions() {
        let fixture = dense_lookup_fixture(2);
        let plans = lookup_plans(&fixture);
        assert_eq!(plans.roots.len(), 2);

        let first = &plans.roots[&fixture.roots[0].1];
        assert_eq!(first.selector, fixture.selector);
        assert_eq!(first.selector_width, 2);
        assert_eq!(first.entries, vec![10, 11, 12, 13]);
        assert_eq!(first.default, fixture.defaults[0]);
        for &(_, compare_idx, concat_idx) in &fixture.conditions {
            assert!(plans.skip_indices.contains(&compare_idx));
            assert!(plans.skip_indices.contains(&concat_idx));
        }
        for indices in &fixture.mux_indices {
            assert!(indices.iter().all(|idx| plans.skip_indices.contains(idx)));
        }
    }

    #[test]
    fn rejects_duplicate_missing_masked_and_mixed_selector_keys() {
        let mut duplicate = dense_lookup_fixture(1);
        let duplicate_key_idx = duplicate.key_defs[3].1;
        duplicate
            .eu
            .blocks
            .get_mut(&SirBlockId(0))
            .unwrap()
            .instructions[duplicate_key_idx] =
            SIRInstruction::Imm(duplicate.key_defs[3].0, SIRValue::new(2u8));
        assert!(lookup_plans(&duplicate).roots.is_empty());

        let mut missing = dense_lookup_fixture(1);
        let missing_key_idx = missing.key_defs[3].1;
        missing
            .eu
            .blocks
            .get_mut(&SirBlockId(0))
            .unwrap()
            .instructions[missing_key_idx] =
            SIRInstruction::Imm(missing.key_defs[3].0, SIRValue::new(4u8));
        assert!(lookup_plans(&missing).roots.is_empty());

        let mut masked = dense_lookup_fixture(1);
        let masked_key_idx = masked.key_defs[2].1;
        masked
            .eu
            .blocks
            .get_mut(&SirBlockId(0))
            .unwrap()
            .instructions[masked_key_idx] =
            SIRInstruction::Imm(masked.key_defs[2].0, SIRValue::new_four_state(2u8, 1u8));
        assert!(lookup_plans(&masked).roots.is_empty());

        let mut mixed = dense_lookup_fixture(1);
        let compare_idx = mixed.conditions[0].1;
        let key = mixed.key_defs[2].0;
        let compare_dst =
            sir_def_reg(&mixed.eu.blocks[&mixed.block_id].instructions[compare_idx]).unwrap();
        mixed
            .eu
            .blocks
            .get_mut(&mixed.block_id)
            .unwrap()
            .instructions[compare_idx] = SIRInstruction::Binary(
            compare_dst,
            mixed.alternate_selector,
            BinaryOp::EqWildcard,
            key,
        );
        assert!(lookup_plans(&mixed).roots.is_empty());
    }

    #[test]
    fn rejects_width_default_and_direction_mismatches() {
        let mut default_width = dense_lookup_fixture(1);
        default_width.eu.register_map.insert(
            default_width.defaults[0],
            RegisterType::Bit {
                width: 7,
                signed: false,
            },
        );
        assert!(lookup_plans(&default_width).roots.is_empty());

        let mut wide_selector = dense_lookup_fixture(1);
        wide_selector.eu.register_map.insert(
            wide_selector.selector,
            RegisterType::Bit {
                width: usize::BITS as usize,
                signed: false,
            },
        );
        for &(key, _) in &wide_selector.key_defs {
            wide_selector.eu.register_map.insert(
                key,
                RegisterType::Bit {
                    width: usize::BITS as usize,
                    signed: false,
                },
            );
        }
        assert!(lookup_plans(&wide_selector).roots.is_empty());

        let mut reversed_wildcard = dense_lookup_fixture(1);
        let compare_idx = reversed_wildcard.conditions[0].1;
        let compare = &mut reversed_wildcard
            .eu
            .blocks
            .get_mut(&reversed_wildcard.block_id)
            .unwrap()
            .instructions[compare_idx];
        let (dst, selector, key) = match compare {
            SIRInstruction::Binary(dst, selector, BinaryOp::EqWildcard, key) => {
                (*dst, *selector, *key)
            }
            _ => unreachable!(),
        };
        *compare = SIRInstruction::Binary(dst, key, BinaryOp::EqWildcard, selector);
        assert!(lookup_plans(&reversed_wildcard).roots.is_empty());

        let mut wide_result = dense_lookup_fixture(1);
        wide_result.eu.register_map.insert(
            wide_result.roots[0].0,
            RegisterType::Bit {
                width: 65,
                signed: false,
            },
        );
        assert!(lookup_plans(&wide_result).roots.is_empty());
    }

    #[test]
    fn group_dce_retains_shared_condition_when_unrecognized_code_uses_it() {
        let mut fixture = dense_lookup_fixture(2);
        let (condition, compare_idx, concat_idx) = fixture.conditions[0];
        let outside = RegisterId(
            fixture
                .eu
                .register_map
                .keys()
                .map(|reg| reg.0)
                .max()
                .unwrap()
                + 1,
        );
        fixture.eu.register_map.insert(
            outside,
            RegisterType::Bit {
                width: 2,
                signed: false,
            },
        );
        fixture
            .eu
            .blocks
            .get_mut(&fixture.block_id)
            .unwrap()
            .instructions
            .push(SIRInstruction::Unary(outside, UnaryOp::Ident, condition));

        let plans = lookup_plans(&fixture);
        assert_eq!(plans.roots.len(), 2);
        assert!(!plans.skip_indices.contains(&concat_idx));
        assert!(!plans.skip_indices.contains(&compare_idx));
    }

    #[test]
    fn group_dce_retains_old_mux_and_its_inputs_for_an_outside_use() {
        let mut fixture = dense_lookup_fixture(1);
        let old_mux_idx = fixture.mux_indices[0][0];
        let old_mux = match fixture.eu.blocks[&fixture.block_id].instructions[old_mux_idx] {
            SIRInstruction::Mux(dst, ..) => dst,
            _ => unreachable!(),
        };
        let outside = RegisterId(
            fixture
                .eu
                .register_map
                .keys()
                .map(|reg| reg.0)
                .max()
                .unwrap()
                + 1,
        );
        fixture.eu.register_map.insert(
            outside,
            RegisterType::Bit {
                width: 8,
                signed: false,
            },
        );
        fixture
            .eu
            .blocks
            .get_mut(&fixture.block_id)
            .unwrap()
            .instructions
            .push(SIRInstruction::Unary(outside, UnaryOp::Ident, old_mux));

        let plans = lookup_plans(&fixture);
        assert_eq!(plans.roots.len(), 1);
        assert!(!plans.skip_indices.contains(&old_mux_idx));
        assert!(!plans.skip_indices.contains(&fixture.conditions[0].2));
    }

    #[test]
    fn lowers_shared_selector_roots_to_cached_indexed_table_loads() {
        let mut fixture = dense_lookup_fixture(2);
        // Make the executable fixture verifier-valid; truncation of the
        // deliberately malformed payload is covered by the recognizer test.
        for inst in &mut fixture
            .eu
            .blocks
            .get_mut(&SirBlockId(0))
            .unwrap()
            .instructions
        {
            if let SIRInstruction::Imm(_, value) = inst
                && value.payload == BigUint::from(0x10du16)
            {
                value.payload = BigUint::from(13u8);
            }
        }

        let input_var = VarId::default();
        let mut first_output_var = input_var;
        first_output_var.inc();
        let mut second_output_var = first_output_var;
        second_output_var.inc();
        let input_abs = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: input_var,
        };
        let first_output_abs = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: first_output_var,
        };
        let second_output_abs = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: second_output_var,
        };
        let input_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, input_abs);
        let first_output_addr =
            RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, first_output_abs);
        let second_output_addr =
            RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, second_output_abs);

        let mut instructions = vec![SIRInstruction::Load(
            fixture.selector,
            input_addr,
            SIROffset::Static(0),
            2,
        )];
        instructions.extend(std::mem::take(
            &mut fixture
                .eu
                .blocks
                .get_mut(&SirBlockId(0))
                .unwrap()
                .instructions,
        ));
        instructions.extend(std::mem::take(
            &mut fixture
                .eu
                .blocks
                .get_mut(&fixture.block_id)
                .unwrap()
                .instructions,
        ));
        instructions.push(SIRInstruction::Store(
            first_output_addr,
            SIROffset::Static(0),
            8,
            fixture.roots[0].0,
            vec![],
            vec![],
        ));
        instructions.push(SIRInstruction::Store(
            second_output_addr,
            SIROffset::Static(0),
            8,
            fixture.roots[1].0,
            vec![],
            vec![],
        ));
        fixture.eu.blocks = [(
            SirBlockId(0),
            BasicBlock {
                id: SirBlockId(0),
                params: vec![],
                instructions,
                terminator: SIRTerminator::Return,
            },
        )]
        .into_iter()
        .collect();
        fixture.eu.entry_block_id = SirBlockId(0);
        fixture.eu.verify();

        let layout = MemoryLayout {
            four_state: false,
            offsets: [
                (input_abs, 0),
                (first_output_abs, 8),
                (second_output_abs, 16),
            ]
            .into_iter()
            .collect(),
            widths: [
                (input_abs, 2),
                (first_output_abs, 8),
                (second_output_abs, 8),
            ]
            .into_iter()
            .collect(),
            is_4states: [
                (input_abs, false),
                (first_output_abs, false),
                (second_output_abs, false),
            ]
            .into_iter()
            .collect(),
            total_size: 24,
            working_offsets: HashMap::default(),
            working_base_offset: 24,
            sparse_offsets: HashMap::default(),
            sparse_base_offset: 24,
            sparse_layouts: HashMap::default(),
            merged_total_size: 24,
            triggered_bits_offset: 24,
            triggered_bits_total_size: 0,
            scratch_base_offset: 24,
            scratch_size: 0,
            runtime_event_capacity: 0,
            runtime_event_slot_size: 0,
            runtime_event_buffer_size: 0,
            runtime_event_site_layouts: vec![],
        };

        let mut function = lower_execution_unit(&fixture.eu, &layout, false);
        function.verify();
        assert_eq!(function.constant_tables().len(), 2);
        assert!(
            function
                .constant_tables()
                .iter()
                .any(|table| table == &[10, 11, 12, 13])
        );
        assert!(
            function
                .constant_tables()
                .iter()
                .any(|table| table == &[26, 27, 28, 29])
        );
        let insts = function.blocks.iter().flat_map(|block| &block.insts);
        let (mut masks, mut scales, mut addresses, mut loads, mut comparisons) = (0, 0, 0, 0, 0);
        for inst in insts {
            match inst {
                MInst::AndImm { imm: 3, .. } => masks += 1,
                MInst::ShlImm { imm: 3, .. } => scales += 1,
                MInst::LoadConstantTableAddr { .. } => addresses += 1,
                MInst::LoadPtrIndexed {
                    size: OpSize::S64, ..
                } => loads += 1,
                MInst::Cmp { .. } | MInst::CmpImm { .. } | MInst::Select { .. } => comparisons += 1,
                _ => {}
            }
        }
        assert_eq!(
            (masks, scales, addresses, loads, comparisons),
            (1, 1, 2, 2, 0)
        );

        mir_legalize::legalize(&mut function);
        function.verify();
        mir_opt::optimize(&mut function);
        function.verify();
        let allocation = regalloc::run_regalloc(&mut function).unwrap();
        mir_opt::post_regalloc_peephole(&mut function);
        function.verify();
        let emitted = emit::emit(
            &function,
            &allocation.assignment,
            allocation.spill_frame_size,
        )
        .unwrap();
        let jit = JitCode::new(&emitted.code).unwrap();
        for selector in 0u8..4 {
            let mut state = vec![0u8; 24];
            state[0] = selector;
            assert_eq!(unsafe { jit.call(&mut state) }, 0);
            assert_eq!(state[8], 10 + selector);
            assert_eq!(state[16], 26 + selector);
        }
    }

    struct CompiledBitCount {
        jit: JitCode,
        state_size: usize,
        input_offset: usize,
        input_bytes: usize,
        input_mask_offset: Option<usize>,
        output_offset: usize,
        output_bytes: usize,
        output_mask_offset: Option<usize>,
    }

    impl CompiledBitCount {
        fn run(&self, value: &BigUint, mask: &BigUint) -> (u64, u64) {
            let mut state = vec![0u8; self.state_size];
            let value_bytes = value.to_bytes_le();
            let value_len = value_bytes.len().min(self.input_bytes);
            state[self.input_offset..self.input_offset + value_len]
                .copy_from_slice(&value_bytes[..value_len]);

            if let Some(input_mask_offset) = self.input_mask_offset {
                let mask_bytes = mask.to_bytes_le();
                let mask_len = mask_bytes.len().min(self.input_bytes);
                state[input_mask_offset..input_mask_offset + mask_len]
                    .copy_from_slice(&mask_bytes[..mask_len]);
            }

            assert_eq!(unsafe { self.jit.call(&mut state) }, 0);

            let read_word = |offset: usize| {
                let mut bytes = [0u8; 8];
                let len = self.output_bytes.min(bytes.len());
                bytes[..len].copy_from_slice(&state[offset..offset + len]);
                u64::from_le_bytes(bytes)
            };
            let result = read_word(self.output_offset);
            let result_mask = self.output_mask_offset.map(read_word).unwrap_or(0);
            (result, result_mask)
        }
    }

    fn compile_bit_count(op: UnaryOp, source_width: usize, four_state: bool) -> CompiledBitCount {
        let result_width = op.result_width(source_width);
        let input_bytes = source_width.div_ceil(8);
        let output_bytes = result_width.div_ceil(8);
        let input_storage_bytes = input_bytes * if four_state { 2 } else { 1 };
        let output_offset = input_storage_bytes.next_multiple_of(8);
        let output_storage_bytes = output_bytes * if four_state { 2 } else { 1 };
        let state_size = (output_offset + output_storage_bytes).max(8);

        let input_var = VarId::default();
        let mut output_var = input_var;
        output_var.inc();
        let input_abs = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: input_var,
        };
        let output_abs = AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: output_var,
        };
        let input_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, input_abs);
        let output_addr = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, output_abs);

        let source = RegisterId(0);
        let result = RegisterId(1);
        let register_type = |width| {
            if four_state {
                RegisterType::Logic { width }
            } else {
                RegisterType::Bit {
                    width,
                    signed: false,
                }
            }
        };
        let eu = ExecutionUnit {
            entry_block_id: SirBlockId(0),
            blocks: [(
                SirBlockId(0),
                BasicBlock {
                    id: SirBlockId(0),
                    params: vec![],
                    instructions: vec![
                        SIRInstruction::Load(
                            source,
                            input_addr,
                            SIROffset::Static(0),
                            source_width,
                        ),
                        SIRInstruction::Unary(result, op, source),
                        SIRInstruction::Store(
                            output_addr,
                            SIROffset::Static(0),
                            result_width,
                            result,
                            vec![],
                            vec![],
                        ),
                    ],
                    terminator: SIRTerminator::Return,
                },
            )]
            .into_iter()
            .collect(),
            register_map: [
                (source, register_type(source_width)),
                (result, register_type(result_width)),
            ]
            .into_iter()
            .collect(),
        };
        eu.verify();

        let layout = MemoryLayout {
            four_state,
            offsets: [(input_abs, 0), (output_abs, output_offset)]
                .into_iter()
                .collect(),
            widths: [(input_abs, source_width), (output_abs, result_width)]
                .into_iter()
                .collect(),
            is_4states: [(input_abs, four_state), (output_abs, four_state)]
                .into_iter()
                .collect(),
            total_size: state_size,
            working_offsets: HashMap::default(),
            working_base_offset: state_size,
            sparse_offsets: HashMap::default(),
            sparse_base_offset: state_size,
            sparse_layouts: HashMap::default(),
            merged_total_size: state_size,
            triggered_bits_offset: state_size,
            triggered_bits_total_size: 0,
            scratch_base_offset: state_size,
            scratch_size: 0,
            runtime_event_capacity: 0,
            runtime_event_slot_size: 0,
            runtime_event_buffer_size: 0,
            runtime_event_site_layouts: vec![],
        };

        let mut function = lower_execution_unit(&eu, &layout, four_state);
        function.verify();
        mir_legalize::legalize(&mut function);
        function.verify();
        mir_opt::optimize(&mut function);
        function.verify();
        let allocation = regalloc::run_regalloc(&mut function).unwrap();
        mir_opt::post_regalloc_peephole(&mut function);
        function.verify();
        let emitted = emit::emit(
            &function,
            &allocation.assignment,
            allocation.spill_frame_size,
        )
        .unwrap();

        CompiledBitCount {
            jit: JitCode::new(&emitted.code).unwrap(),
            state_size,
            input_offset: 0,
            input_bytes,
            input_mask_offset: four_state.then_some(input_bytes),
            output_offset,
            output_bytes,
            output_mask_offset: four_state.then_some(output_offset + output_bytes),
        }
    }

    fn assert_bit_counts(
        source_width: usize,
        cases: impl IntoIterator<Item = (BigUint, u64, u64, u64)>,
    ) {
        let popcount = compile_bit_count(UnaryOp::PopCount, source_width, false);
        let leading = compile_bit_count(UnaryOp::CountLeadingZeros, source_width, false);
        let trailing = compile_bit_count(UnaryOp::CountTrailingZeros, source_width, false);
        for (value, expected_popcount, expected_leading, expected_trailing) in cases {
            assert_eq!(
                popcount.run(&value, &BigUint::from(0u8)),
                (expected_popcount, 0),
                "popcount width={source_width} value={value:#x}"
            );
            assert_eq!(
                leading.run(&value, &BigUint::from(0u8)),
                (expected_leading, 0),
                "clz width={source_width} value={value:#x}"
            );
            assert_eq!(
                trailing.run(&value, &BigUint::from(0u8)),
                (expected_trailing, 0),
                "ctz width={source_width} value={value:#x}"
            );
        }
    }

    #[test]
    fn full_static_native_access_must_fit_allocated_bytes_exactly() {
        for (width, expected) in [
            (0, None),
            (17, None),
            (24, None),
            (33, None),
            (40, None),
            (56, None),
            (65, None),
            (31, Some(OpSize::S32)),
            (32, Some(OpSize::S32)),
            (63, Some(OpSize::S64)),
            (64, Some(OpSize::S64)),
        ] {
            assert_eq!(
                ISelContext::exact_storage_access_size(width),
                expected,
                "width={width}"
            );
        }
    }

    #[test]
    fn native_bit_counts_cover_one_to_sixty_four_bits_and_zero() {
        for source_width in 1..=64 {
            let top = BigUint::from(1u8) << (source_width - 1);
            let edge_bits = if source_width == 1 {
                top
            } else {
                top | BigUint::from(1u8)
            };
            let edge_popcount = if source_width == 1 { 1 } else { 2 };
            assert_bit_counts(
                source_width,
                [
                    (
                        BigUint::from(0u8),
                        0,
                        source_width as u64,
                        source_width as u64,
                    ),
                    (edge_bits, edge_popcount, 0, 0),
                ],
            );
        }

        assert_bit_counts(
            7,
            [
                (BigUint::from(0b001_0100u8), 2, 2, 2),
                (BigUint::from(0b100_0000u8), 1, 0, 6),
            ],
        );
        assert_bit_counts(
            64,
            [
                (BigUint::from(1u64), 1, 63, 0),
                (BigUint::from(1u64 << 63), 1, 0, 63),
                (BigUint::from(u64::MAX), 64, 0, 0),
            ],
        );
    }

    #[test]
    fn native_bit_counts_cover_wide_and_partial_top_chunks() {
        let bit64 = BigUint::from(1u8) << 64usize;
        assert_bit_counts(
            65,
            [
                (BigUint::from(0u8), 0, 65, 65),
                (BigUint::from(1u8), 1, 64, 0),
                (bit64.clone(), 1, 0, 64),
                (bit64 | BigUint::from(1u8), 2, 0, 0),
            ],
        );

        let mixed = (BigUint::from(1u8) << 129usize)
            | (BigUint::from(1u8) << 64usize)
            | (BigUint::from(1u8) << 3usize);
        let middle = BigUint::from(1u8) << 64usize;
        assert_bit_counts(
            130,
            [
                (BigUint::from(0u8), 0, 130, 130),
                (mixed, 3, 0, 3),
                (middle, 1, 65, 64),
            ],
        );
    }

    #[test]
    fn native_wide_bit_counts_produce_conservative_x_results() {
        let unknown = BigUint::from(1u8) << 64usize;
        for op in [
            UnaryOp::PopCount,
            UnaryOp::CountLeadingZeros,
            UnaryOp::CountTrailingZeros,
        ] {
            let compiled = compile_bit_count(op, 65, true);
            assert_eq!(
                compiled.run(&BigUint::from(0u8), &unknown),
                (0x7f, 0x7f),
                "{op}"
            );
        }
    }
}
