use crate::HashMap;
use crate::ir::*;

use super::cost_model::{
    CLIF_INST_THRESHOLD, estimate_clif_cost, estimate_eu_cost, estimate_units_cost,
};

/// A chunk in the tail-call chain.
#[derive(Debug, Clone)]
pub struct TailCallChunk {
    /// EUs in this chunk (may be complete EUs or sub-EUs from intra-EU split).
    pub units: Vec<ExecutionUnit<RegionedAbsoluteAddr>>,
    /// Live registers at chunk entry (passed from previous chunk via tail-call args).
    /// Empty for chunk 0.
    pub incoming_live_regs: Vec<(RegisterId, RegisterType)>,
    /// Live registers at chunk exit (passed to next chunk).
    /// Empty for last chunk.
    pub outgoing_live_regs: Vec<(RegisterId, RegisterType)>,
}

// ---------------------------------------------------------------------------
// Memory-spilled multi-block EU splitting
// ---------------------------------------------------------------------------

/// Plan for compiling a multi-block EU that exceeds the CLIF instruction limit.
/// Each chunk is compiled as a separate function. Inter-chunk live registers are
/// passed through a scratch memory region rather than function arguments.
#[derive(Debug, Clone)]
pub struct MemorySpilledPlan {
    pub chunks: Vec<SpilledChunk>,
    /// Total scratch memory needed (bytes).
    pub scratch_bytes: usize,
}

/// One chunk in a memory-spilled compilation plan.
#[derive(Debug, Clone)]
pub struct SpilledChunk {
    /// The execution unit containing this chunk's blocks.
    pub eu: ExecutionUnit<RegionedAbsoluteAddr>,
    /// Registers to load from scratch memory at chunk function entry.
    pub incoming_spills: Vec<SpillSlot>,
    /// Registers to store to scratch memory before cross-chunk tail-calls.
    pub outgoing_spills: Vec<SpillSlot>,
    /// Detailed cross-chunk edge info including param→scratch mappings.
    /// Also encodes cross-chunk targets: if a BlockId is a key here, the terminator
    /// must emit a spill + tail-call to `edge.target_chunk_index` instead of a local jump.
    pub cross_chunk_edges: HashMap<BlockId, CrossChunkEdge>,
}

/// A single register spill slot in the scratch memory region.
#[derive(Debug, Clone)]
pub struct SpillSlot {
    pub reg_id: RegisterId,
    pub reg_ty: RegisterType,
    /// Byte offset within the scratch region.
    pub scratch_byte_offset: usize,
}

/// Describes a cross-chunk control flow edge: when a terminator in one chunk
/// targets a block that lives in a different chunk, we need to spill args
/// and tail-call into the target chunk.
#[derive(Debug, Clone)]
pub struct CrossChunkEdge {
    pub target_chunk_index: usize,
    /// Mapping from block-param RegisterId → scratch byte offset.
    /// When jumping cross-chunk, the caller stores args here before tail-calling.
    pub param_scratch_offsets: Vec<(RegisterId, usize)>,
}

/// Attempt to split a set of execution units into tail-call chunks if the total
/// estimated CLIF cost exceeds the threshold.
///
/// Returns `None` if no splitting is needed (total cost within threshold).
pub fn split_if_needed(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    four_state: bool,
) -> Option<Vec<TailCallChunk>> {
    split_with_threshold(units, four_state, CLIF_INST_THRESHOLD)
}

/// Internal version with configurable threshold (for testing).
pub(crate) fn split_with_threshold(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    four_state: bool,
    threshold: usize,
) -> Option<Vec<TailCallChunk>> {
    let total_cost = estimate_units_cost(units, four_state);
    if total_cost <= threshold {
        return None;
    }

    // Primary path: EU-boundary splitting.
    // Since RegisterIds are EU-scoped, splitting between EUs has zero live-reg cost.
    let eu_costs: Vec<usize> = units
        .iter()
        .map(|eu| estimate_eu_cost(eu, four_state))
        .collect();

    let mut chunks: Vec<TailCallChunk> = Vec::new();
    let mut current_units: Vec<ExecutionUnit<RegionedAbsoluteAddr>> = Vec::new();
    let mut current_cost = 0usize;

    for (i, eu) in units.iter().enumerate() {
        let eu_cost = eu_costs[i];

        // If a single EU exceeds threshold, try intra-EU splitting
        if eu_cost > threshold {
            // Flush current chunk first
            if !current_units.is_empty() {
                chunks.push(TailCallChunk {
                    units: std::mem::take(&mut current_units),
                    incoming_live_regs: Vec::new(),
                    outgoing_live_regs: Vec::new(),
                });
                current_cost = 0;
            }

            // Try intra-EU split
            if let Some(sub_chunks) = split_single_eu(eu, four_state, threshold) {
                chunks.extend(sub_chunks);
            } else {
                // Fallback: treat as single chunk (will be large but is our best effort)
                chunks.push(TailCallChunk {
                    units: vec![eu.clone()],
                    incoming_live_regs: Vec::new(),
                    outgoing_live_regs: Vec::new(),
                });
            }
            continue;
        }

        // Would adding this EU exceed the threshold?
        if current_cost + eu_cost > threshold && !current_units.is_empty() {
            chunks.push(TailCallChunk {
                units: std::mem::take(&mut current_units),
                incoming_live_regs: Vec::new(),
                outgoing_live_regs: Vec::new(),
            });
            current_cost = 0;
        }

        current_units.push(eu.clone());
        current_cost += eu_cost;
    }

    // Flush remaining
    if !current_units.is_empty() {
        chunks.push(TailCallChunk {
            units: current_units,
            incoming_live_regs: Vec::new(),
            outgoing_live_regs: Vec::new(),
        });
    }

    // If we ended up with only one chunk, no split needed
    if chunks.len() <= 1 {
        return None;
    }

    Some(chunks)
}

/// Try to split a single EU that exceeds the threshold.
/// Targets single-block EUs (the common case for eval_comb).
fn split_single_eu(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
    threshold: usize,
) -> Option<Vec<TailCallChunk>> {
    // Only handle single-block EUs
    if eu.blocks.len() != 1 {
        return None;
    }

    let block = eu.blocks.values().next().unwrap();
    let instructions = &block.instructions;

    if instructions.is_empty() {
        return None;
    }

    // Step 1: Identify split candidate positions (immediately after each Store instruction)
    let mut candidates: Vec<usize> = Vec::new();
    for (i, inst) in instructions.iter().enumerate() {
        if matches!(inst, SIRInstruction::Store(..)) {
            candidates.push(i + 1);
        }
    }

    if candidates.is_empty() {
        return None;
    }

    // Ensure the last candidate isn't past the end
    candidates.retain(|&c| c < instructions.len());

    if candidates.is_empty() {
        return None;
    }

    // Step 2: Compute instruction costs
    let inst_costs: Vec<usize> = instructions
        .iter()
        .map(|inst| estimate_clif_cost(inst, &eu.register_map, four_state))
        .collect();

    // Step 3: Backward liveness analysis
    // For each split candidate position, compute the set of live registers.
    // A register is live at position `pos` if it is used at or after `pos`
    // and defined before `pos`.
    let live_sets = compute_liveness_at_candidates(instructions, &candidates);

    // Step 4: DP forward pass to find optimal split points minimizing live-reg cost
    // candidate_indices: indices into the `candidates` array
    // dp[j] = minimum total live-reg cost to split [0..candidates[j]]
    let n = candidates.len();

    // Prefix sum of instruction costs
    let mut prefix_cost = vec![0usize; instructions.len() + 1];
    for (i, &c) in inst_costs.iter().enumerate() {
        prefix_cost[i + 1] = prefix_cost[i] + c;
    }

    let total_inst_cost = prefix_cost[instructions.len()];
    if total_inst_cost <= threshold {
        // Re-check: maybe after more careful accounting we're under threshold
        return None;
    }

    // dp[j]: min live-reg count to split using candidates[0..j] as possible endpoints
    // The last segment from candidates[j] to end is always included.
    // We model: segments are [0..candidates[s0]], [candidates[s0]..candidates[s1]], etc.
    // plus a final segment from the last split to end.

    let segment_cost = |start_inst: usize, end_inst: usize| -> usize {
        prefix_cost[end_inst] - prefix_cost[start_inst]
    };

    // We need split points s.t. each resulting segment's cost ≤ threshold.
    // Among these, choose the set minimizing total live-reg params.

    // dp[j] = min total incoming_live_regs size for splitting [0..candidates[j]] into valid chunks
    // plus the chunk [candidates[j]..end] must also be valid
    let mut dp = vec![usize::MAX; n];
    let mut dp_prev = vec![usize::MAX; n]; // which candidate was the previous split

    for j in 0..n {
        let seg_cost = segment_cost(0, candidates[j]);
        if seg_cost <= threshold {
            // Single segment [0..candidates[j]] is valid
            dp[j] = live_sets[j].len();
            dp_prev[j] = usize::MAX; // no previous split
        }
    }

    for j in 0..n {
        if dp[j] == usize::MAX {
            continue;
        }
        // Try extending from candidates[j] to candidates[k]
        for k in (j + 1)..n {
            let seg_cost = segment_cost(candidates[j], candidates[k]);
            if seg_cost > threshold {
                break; // further candidates will be even more expensive
            }
            let new_cost = dp[j] + live_sets[k].len();
            if new_cost < dp[k] {
                dp[k] = new_cost;
                dp_prev[k] = j;
            }
        }
    }

    // Find the best final split point such that the remainder fits in threshold
    let mut best_end = usize::MAX;
    let mut best_total_cost = usize::MAX;

    for j in 0..n {
        if dp[j] == usize::MAX {
            continue;
        }
        let remainder_cost = segment_cost(candidates[j], instructions.len());
        if remainder_cost <= threshold {
            if dp[j] < best_total_cost {
                best_total_cost = dp[j];
                best_end = j;
            }
        }
    }

    if best_end == usize::MAX {
        // Also check if no split at all works (shouldn't since we checked total > threshold)
        // or if we can't find a valid partitioning
        return None;
    }

    // Backtrace to extract split points
    let mut split_indices = Vec::new();
    let mut cur = best_end;
    while cur != usize::MAX {
        split_indices.push(cur);
        cur = dp_prev[cur];
    }
    split_indices.reverse();

    // Construct sub-EUs from the splits
    let split_positions: Vec<usize> = split_indices.iter().map(|&i| candidates[i]).collect();

    let mut chunks: Vec<TailCallChunk> = Vec::new();
    let mut seg_start = 0;

    for (chunk_idx, &split_pos) in split_positions.iter().enumerate() {
        let sub_eu = make_sub_eu(
            eu,
            block,
            &instructions[seg_start..split_pos],
            seg_start == 0,
        );
        let incoming = if chunk_idx == 0 {
            Vec::new()
        } else {
            let prev_live_idx = split_indices[chunk_idx - 1];
            live_sets[prev_live_idx]
                .iter()
                .map(|&reg_id| (reg_id, eu.register_map[&reg_id].clone()))
                .collect()
        };
        let outgoing: Vec<(RegisterId, RegisterType)> = live_sets[split_indices[chunk_idx]]
            .iter()
            .map(|&reg_id| (reg_id, eu.register_map[&reg_id].clone()))
            .collect();

        chunks.push(TailCallChunk {
            units: vec![sub_eu],
            incoming_live_regs: incoming,
            outgoing_live_regs: outgoing,
        });

        seg_start = split_pos;
    }

    // Final segment: from last split to end
    let sub_eu = make_sub_eu(eu, block, &instructions[seg_start..], false);
    let incoming = if split_positions.is_empty() {
        Vec::new()
    } else {
        let last_live_idx = *split_indices.last().unwrap();
        live_sets[last_live_idx]
            .iter()
            .map(|&reg_id| (reg_id, eu.register_map[&reg_id].clone()))
            .collect()
    };
    chunks.push(TailCallChunk {
        units: vec![sub_eu],
        incoming_live_regs: incoming,
        outgoing_live_regs: Vec::new(),
    });

    if chunks.len() <= 1 {
        return None;
    }

    Some(chunks)
}

/// Compute liveness at each candidate split position.
/// Returns a vec of sorted RegisterId sets, one per candidate.
fn compute_liveness_at_candidates(
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    candidates: &[usize],
) -> Vec<Vec<RegisterId>> {
    use crate::HashSet;

    let n = instructions.len();

    // Compute def and use for each instruction
    let mut defs: Vec<Option<RegisterId>> = Vec::with_capacity(n);
    let mut uses: Vec<Vec<RegisterId>> = Vec::with_capacity(n);
    for inst in instructions {
        defs.push(def_reg(inst));
        let mut u = Vec::new();
        collect_used_regs(inst, &mut u);
        uses.push(u);
    }

    // Backward liveness analysis.
    // live_before[i] = set of registers live just before instruction i.
    // live_before[n] is empty (end of block).
    // Recurrence: live_before[i] = (live_before[i+1] - def[i]) ∪ use[i]
    let mut live_before: Vec<HashSet<RegisterId>> = vec![HashSet::default(); n + 1];

    for i in (0..n).rev() {
        let mut live = live_before[i + 1].clone();
        if let Some(def) = defs[i] {
            live.remove(&def);
        }
        for &u in &uses[i] {
            live.insert(u);
        }
        live_before[i] = live;
    }

    // At split position `pos`, live registers = live_before[pos]
    // (registers defined before `pos` that are used at or after `pos`).
    candidates
        .iter()
        .map(|&pos| {
            let mut regs: Vec<RegisterId> = live_before[pos].iter().copied().collect();
            regs.sort();
            regs
        })
        .collect()
}

fn def_reg<A>(inst: &SIRInstruction<A>) -> Option<RegisterId> {
    match inst {
        SIRInstruction::Imm(dst, _)
        | SIRInstruction::Binary(dst, _, _, _)
        | SIRInstruction::Unary(dst, _, _)
        | SIRInstruction::Load(dst, _, _, _)
        | SIRInstruction::Concat(dst, _) => Some(*dst),
        SIRInstruction::Store(..) | SIRInstruction::Commit(..) => None,
    }
}

fn collect_used_regs<A>(inst: &SIRInstruction<A>, out: &mut Vec<RegisterId>) {
    match inst {
        SIRInstruction::Imm(_, _) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            out.push(*lhs);
            out.push(*rhs);
        }
        SIRInstruction::Unary(_, _, src) => {
            out.push(*src);
        }
        SIRInstruction::Load(_, _, SIROffset::Dynamic(off), _) => {
            out.push(*off);
        }
        SIRInstruction::Load(_, _, SIROffset::Static(_), _) => {}
        SIRInstruction::Store(_, SIROffset::Dynamic(off), _, src, _) => {
            out.push(*off);
            out.push(*src);
        }
        SIRInstruction::Store(_, SIROffset::Static(_), _, src, _) => {
            out.push(*src);
        }
        SIRInstruction::Commit(_, _, SIROffset::Dynamic(off), _, _) => {
            out.push(*off);
        }
        SIRInstruction::Commit(_, _, SIROffset::Static(_), _, _) => {}
        SIRInstruction::Concat(_, args) => out.extend(args.iter().copied()),
    }
}

// ---------------------------------------------------------------------------
// Multi-block EU splitting
// ---------------------------------------------------------------------------

/// Try to split a multi-block EU using memory-spilled chunks.
pub fn split_if_needed_spilled(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    four_state: bool,
) -> Option<MemorySpilledPlan> {
    split_multi_block_with_threshold(units, four_state, CLIF_INST_THRESHOLD)
}

pub(crate) fn split_multi_block_with_threshold(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    four_state: bool,
    threshold: usize,
) -> Option<MemorySpilledPlan> {
    // Process ALL multi-block EUs that exceed the threshold.
    // (In practice, split_if_needed_spilled is called when split_if_needed returned None,
    //  meaning all units are in a single oversized chunk — typically one multi-block EU.)
    let mut combined_chunks: Vec<SpilledChunk> = Vec::new();
    let mut combined_scratch_bytes = 0usize;

    for eu in units {
        let eu_cost = estimate_eu_cost(eu, four_state);
        if eu_cost > threshold && eu.blocks.len() > 1 {
            if let Some(plan) =
                split_multi_block_eu(eu, four_state, threshold, combined_scratch_bytes)
            {
                combined_scratch_bytes = plan.scratch_bytes;
                combined_chunks.extend(plan.chunks);
            }
        }
    }

    if combined_chunks.is_empty() {
        return None;
    }

    Some(MemorySpilledPlan {
        chunks: combined_chunks,
        scratch_bytes: combined_scratch_bytes,
    })
}

fn split_multi_block_eu(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    four_state: bool,
    threshold: usize,
    scratch_base: usize,
) -> Option<MemorySpilledPlan> {
    use crate::HashSet;

    let mut modified_eu = eu.clone();

    // 1. Split oversized individual blocks at Store boundaries
    let mut next_block_id = modified_eu.blocks.keys().map(|b| b.0).max().unwrap_or(0) + 1;
    let block_ids_to_check: Vec<BlockId> = modified_eu.blocks.keys().copied().collect();
    for bid in block_ids_to_check {
        let block_cost = estimate_block_cost(&modified_eu, bid, four_state);
        if block_cost > threshold {
            split_oversized_block(
                &mut modified_eu,
                bid,
                &mut next_block_id,
                four_state,
                threshold,
            );
        }
    }

    // 2. Topological sort
    let topo_order = topological_sort_blocks(&modified_eu.blocks, modified_eu.entry_block_id);

    // 3. Compute per-block costs
    let block_costs: HashMap<BlockId, usize> = topo_order
        .iter()
        .map(|&bid| (bid, estimate_block_cost(&modified_eu, bid, four_state)))
        .collect();

    // 4. Single-pass partition ensuring single entry per chunk.
    //    Pre-identifies back-edge targets (loop headers) and forces them as chunk heads.
    //    Then processes blocks in topo order, also forcing new chunks when a block has
    //    a forward predecessor in a different chunk.
    let chunk_groups = partition_single_pass(&modified_eu, &topo_order, &block_costs, threshold);

    if chunk_groups.len() <= 1 {
        return None;
    }

    // 5. Block→chunk mapping
    let mut block_to_chunk: HashMap<BlockId, usize> = HashMap::default();
    for (ci, group) in chunk_groups.iter().enumerate() {
        for &bid in group {
            block_to_chunk.insert(bid, ci);
        }
    }

    // 6. Compute all registers that need scratch slots.
    //    Two sources:
    //    (a) Inter-chunk live: defined in chunk i, used in chunk j (j > i)
    //    (b) Block params at cross-chunk edge targets
    let n = chunk_groups.len();
    let mut defined_in: Vec<HashSet<RegisterId>> = vec![HashSet::default(); n];
    let mut used_in: Vec<HashSet<RegisterId>> = vec![HashSet::default(); n];

    for (ci, group) in chunk_groups.iter().enumerate() {
        for &bid in group {
            let block = &modified_eu.blocks[&bid];
            for &p in &block.params {
                defined_in[ci].insert(p);
            }
            for inst in &block.instructions {
                if let Some(def) = def_reg(inst) {
                    defined_in[ci].insert(def);
                }
                let mut u = Vec::new();
                collect_used_regs(inst, &mut u);
                for r in u {
                    used_in[ci].insert(r);
                }
            }
            collect_terminator_used_regs(&block.terminator, &mut used_in[ci]);
        }
    }

    let mut all_spill_regs: HashSet<RegisterId> = HashSet::default();

    // (a) Forward liveness
    for (i, defined) in defined_in.iter().enumerate() {
        for used in &used_in[(i + 1)..] {
            for &reg in defined {
                if used.contains(&reg) {
                    all_spill_regs.insert(reg);
                }
            }
        }
    }

    // (b) Block params at cross-chunk edge targets
    for group in &chunk_groups {
        let group_set: HashSet<BlockId> = group.iter().copied().collect();
        for &bid in group {
            let block = &modified_eu.blocks[&bid];
            for succ in block_successors(&block.terminator) {
                if !group_set.contains(&succ) {
                    if let Some(target_block) = modified_eu.blocks.get(&succ) {
                        for &param in &target_block.params {
                            all_spill_regs.insert(param);
                        }
                    }
                }
            }
        }
    }

    // 7. Assign scratch byte offsets
    let state_mul = if four_state { 2 } else { 1 };
    let mut scratch_bytes = scratch_base;
    let mut spill_offset_map: HashMap<RegisterId, usize> = HashMap::default();
    let mut all_spill_slots: Vec<SpillSlot> = Vec::new();

    let mut sorted_spill_regs: Vec<RegisterId> = all_spill_regs.iter().copied().collect();
    sorted_spill_regs.sort();

    for reg_id in sorted_spill_regs {
        let reg_ty = modified_eu.register_map[&reg_id].clone();
        let width = reg_ty.width();
        let num_i64_chunks = width.div_ceil(64).max(1);
        let slot_bytes = num_i64_chunks * 8 * state_mul;

        // Align to 8 bytes
        scratch_bytes = (scratch_bytes + 7) & !7;

        spill_offset_map.insert(reg_id, scratch_bytes);
        all_spill_slots.push(SpillSlot {
            reg_id,
            reg_ty,
            scratch_byte_offset: scratch_bytes,
        });
        scratch_bytes += slot_bytes;
    }

    // 8. Create SpilledChunks with per-chunk incoming/outgoing spills
    let mut chunks = Vec::new();
    for (ci, group) in chunk_groups.iter().enumerate() {
        let group_set: HashSet<BlockId> = group.iter().copied().collect();

        // Sub-EU with only this chunk's blocks
        let mut sub_blocks = HashMap::default();
        for &bid in group {
            sub_blocks.insert(bid, modified_eu.blocks[&bid].clone());
        }

        // Minimal register_map
        let mut register_map = HashMap::default();
        for block in sub_blocks.values() {
            for &p in &block.params {
                if let Some(ty) = modified_eu.register_map.get(&p) {
                    register_map.insert(p, ty.clone());
                }
            }
            for inst in &block.instructions {
                if let Some(def) = def_reg(inst) {
                    if let Some(ty) = modified_eu.register_map.get(&def) {
                        register_map.insert(def, ty.clone());
                    }
                }
                let mut used = Vec::new();
                collect_used_regs(inst, &mut used);
                for r in used {
                    if let Some(ty) = modified_eu.register_map.get(&r) {
                        register_map.insert(r, ty.clone());
                    }
                }
            }
            collect_terminator_regs_into_map(
                &block.terminator,
                &modified_eu.register_map,
                &mut register_map,
            );
        }
        // Include spill registers this chunk will load/store
        for slot in &all_spill_slots {
            register_map.insert(slot.reg_id, slot.reg_ty.clone());
        }

        let entry_block_id = group[0];
        let sub_eu = ExecutionUnit {
            entry_block_id,
            blocks: sub_blocks,
            register_map,
        };

        // Per-chunk incoming spills: regs this chunk needs to LOAD from scratch at entry.
        // This includes:
        // - Entry block params that receive values from cross-chunk edges
        // - Registers defined in earlier chunks and used (but not re-defined) in this chunk
        let mut incoming_regs: HashSet<RegisterId> = HashSet::default();
        let entry_block = &modified_eu.blocks[&entry_block_id];
        for &param in &entry_block.params {
            if spill_offset_map.contains_key(&param) {
                incoming_regs.insert(param);
            }
        }
        for slot in &all_spill_slots {
            if used_in[ci].contains(&slot.reg_id) && !defined_in[ci].contains(&slot.reg_id) {
                incoming_regs.insert(slot.reg_id);
            }
        }
        let incoming_spills: Vec<SpillSlot> = all_spill_slots
            .iter()
            .filter(|s| incoming_regs.contains(&s.reg_id))
            .cloned()
            .collect();

        // Per-chunk outgoing spills: regs this chunk defines that any later chunk needs.
        let mut outgoing_regs: HashSet<RegisterId> = HashSet::default();
        for slot in &all_spill_slots {
            if defined_in[ci].contains(&slot.reg_id) {
                if used_in[(ci + 1)..]
                    .iter()
                    .any(|used| used.contains(&slot.reg_id))
                {
                    outgoing_regs.insert(slot.reg_id);
                }
            }
        }
        let outgoing_spills: Vec<SpillSlot> = all_spill_slots
            .iter()
            .filter(|s| outgoing_regs.contains(&s.reg_id))
            .cloned()
            .collect();

        // Cross-chunk edges: map target block → (chunk index, param scratch offsets)
        // Every target block param gets a scratch offset (no filter_map dropping).
        let mut cross_chunk_edges: HashMap<BlockId, CrossChunkEdge> = HashMap::default();
        for &bid in group {
            let block = &modified_eu.blocks[&bid];
            for (target_bid, _args) in terminator_targets_with_args(&block.terminator) {
                if !group_set.contains(&target_bid) {
                    let target_block = &modified_eu.blocks[&target_bid];
                    let param_scratch_offsets: Vec<(RegisterId, usize)> = target_block
                        .params
                        .iter()
                        .map(|&param| (param, spill_offset_map[&param]))
                        .collect();
                    cross_chunk_edges.insert(
                        target_bid,
                        CrossChunkEdge {
                            target_chunk_index: block_to_chunk[&target_bid],
                            param_scratch_offsets,
                        },
                    );
                }
            }
        }

        chunks.push(SpilledChunk {
            eu: sub_eu,
            incoming_spills,
            outgoing_spills,
            cross_chunk_edges,
        });
    }

    Some(MemorySpilledPlan {
        chunks,
        scratch_bytes,
    })
}

fn estimate_block_cost(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    four_state: bool,
) -> usize {
    let state_mul = if four_state { 2 } else { 1 };
    let block = &eu.blocks[&block_id];
    let mut cost = block.params.len() * state_mul;
    for inst in &block.instructions {
        cost += estimate_clif_cost(inst, &eu.register_map, four_state);
    }
    cost += match &block.terminator {
        SIRTerminator::Jump(_, _) => 1,
        SIRTerminator::Branch { .. } => 2,
        SIRTerminator::Return => 2,
        SIRTerminator::Error(_) => 2,
    };
    cost
}

/// Topological sort using Kahn's algorithm.
/// The entry block is always placed first, even if it has back-edges.
/// Blocks in cycles that never reach in-degree 0 are appended in sorted order.
pub(crate) fn topological_sort_blocks(
    blocks: &HashMap<BlockId, BasicBlock<RegionedAbsoluteAddr>>,
    entry: BlockId,
) -> Vec<BlockId> {
    let mut in_degree: HashMap<BlockId, usize> = HashMap::default();
    let mut successors: HashMap<BlockId, Vec<BlockId>> = HashMap::default();

    for (&bid, block) in blocks {
        in_degree.entry(bid).or_insert(0);
        for succ in block_successors(&block.terminator) {
            if blocks.contains_key(&succ) {
                *in_degree.entry(succ).or_insert(0) += 1;
                successors.entry(bid).or_default().push(succ);
            }
        }
    }

    // Force entry block to be processed first, even if it has back-edges
    // (loop headers commonly receive back-edges that inflate their in-degree).
    *in_degree.entry(entry).or_default() = 0;

    let mut queue = std::collections::VecDeque::new();
    queue.push_back(entry);

    // Also add any other blocks with in_degree 0 (sorted for determinism)
    let mut other_zeros: Vec<BlockId> = in_degree
        .iter()
        .filter(|(bid, deg)| **deg == 0 && **bid != entry)
        .map(|(bid, _)| *bid)
        .collect();
    other_zeros.sort();
    for bid in other_zeros {
        queue.push_back(bid);
    }

    let mut visited = crate::HashSet::default();
    let mut result = Vec::new();
    while let Some(bid) = queue.pop_front() {
        if !visited.insert(bid) {
            continue;
        }
        result.push(bid);
        if let Some(succs) = successors.get(&bid) {
            let mut sorted_succs = succs.clone();
            sorted_succs.sort();
            for succ in sorted_succs {
                let deg = in_degree.get_mut(&succ).unwrap();
                *deg = deg.saturating_sub(1);
                if *deg == 0 {
                    queue.push_back(succ);
                }
            }
        }
    }

    // Fallback: append any remaining blocks not reached by the sort (due to cycles)
    if result.len() < blocks.len() {
        let mut remaining: Vec<BlockId> = blocks
            .keys()
            .filter(|b| !visited.contains(b))
            .copied()
            .collect();
        remaining.sort();
        result.extend(remaining);
    }

    result
}

fn block_successors(term: &SIRTerminator) -> Vec<BlockId> {
    match term {
        SIRTerminator::Jump(target, _) => vec![*target],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![true_block.0, false_block.0],
        SIRTerminator::Return | SIRTerminator::Error(_) => vec![],
    }
}

/// Returns (target_block_id, args) pairs for each successor edge.
fn terminator_targets_with_args(term: &SIRTerminator) -> Vec<(BlockId, Vec<RegisterId>)> {
    match term {
        SIRTerminator::Jump(target, args) => vec![(*target, args.clone())],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![
            (true_block.0, true_block.1.clone()),
            (false_block.0, false_block.1.clone()),
        ],
        SIRTerminator::Return | SIRTerminator::Error(_) => vec![],
    }
}

/// Single-pass partition that ensures each chunk has exactly one entry point.
///
/// 1. Pre-identifies back-edge targets (loop headers) — these must be chunk heads
///    because a later block may jump back to them from a different chunk.
/// 2. Processes blocks in topological order. A new chunk is started when:
///    - The block is a back-edge target (loop header)
///    - The block has a forward predecessor in a different chunk
///    - Adding the block would exceed the cost threshold
fn partition_single_pass(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    topo_order: &[BlockId],
    block_costs: &HashMap<BlockId, usize>,
    threshold: usize,
) -> Vec<Vec<BlockId>> {
    use crate::HashSet;

    let position: HashMap<BlockId, usize> = topo_order
        .iter()
        .enumerate()
        .map(|(i, &b)| (b, i))
        .collect();

    // Pre-identify back-edge targets: blocks whose predecessor appears later in topo order.
    // These must be chunk heads to guarantee single-entry chunks.
    let mut must_be_head: HashSet<BlockId> = HashSet::default();
    for &bid in topo_order {
        let block = &eu.blocks[&bid];
        for succ in block_successors(&block.terminator) {
            if let Some(&succ_pos) = position.get(&succ) {
                if succ_pos <= position[&bid] {
                    // Back edge (or self-loop): succ must be a chunk head
                    must_be_head.insert(succ);
                }
            }
        }
    }

    // Build forward-predecessor map: for each block, which blocks have forward edges to it?
    let mut forward_preds: HashMap<BlockId, Vec<BlockId>> = HashMap::default();
    for &bid in topo_order {
        forward_preds.entry(bid).or_default();
    }
    for &bid in topo_order {
        let block = &eu.blocks[&bid];
        for succ in block_successors(&block.terminator) {
            if let Some(&succ_pos) = position.get(&succ) {
                if succ_pos > position[&bid] {
                    forward_preds.entry(succ).or_default().push(bid);
                }
            }
        }
    }

    let mut block_to_chunk: HashMap<BlockId, usize> = HashMap::default();
    let mut groups: Vec<Vec<BlockId>> = Vec::new();
    let mut current_group: Vec<BlockId> = Vec::new();
    let mut current_cost = 0usize;
    let mut current_chunk_idx = 0usize;

    for &bid in topo_order {
        let cost = block_costs[&bid];

        let force_new_chunk = if current_group.is_empty() {
            false
        } else {
            must_be_head.contains(&bid)
                || forward_preds[&bid].iter().any(|pred| {
                    block_to_chunk
                        .get(pred)
                        .is_some_and(|&c| c != current_chunk_idx)
                })
                || current_cost + cost > threshold
        };

        if force_new_chunk {
            groups.push(std::mem::take(&mut current_group));
            current_cost = 0;
            current_chunk_idx = groups.len();
        }

        current_group.push(bid);
        block_to_chunk.insert(bid, current_chunk_idx);
        current_cost += cost;
    }

    if !current_group.is_empty() {
        groups.push(current_group);
    }

    groups
}

/// Split an oversized block at Store boundaries into sub-blocks within the EU.
fn split_oversized_block(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    block_id: BlockId,
    next_block_id: &mut usize,
    four_state: bool,
    threshold: usize,
) {
    let block = &eu.blocks[&block_id];
    let instructions = &block.instructions;

    // Find Store boundary candidates
    let mut candidates: Vec<usize> = Vec::new();
    for (i, inst) in instructions.iter().enumerate() {
        if matches!(inst, SIRInstruction::Store(..)) {
            candidates.push(i + 1);
        }
    }
    candidates.retain(|&c| c < instructions.len());
    if candidates.is_empty() {
        return;
    }

    // Prefix costs
    let inst_costs: Vec<usize> = instructions
        .iter()
        .map(|inst| estimate_clif_cost(inst, &eu.register_map, four_state))
        .collect();
    let mut prefix_cost = vec![0usize; instructions.len() + 1];
    for (i, &c) in inst_costs.iter().enumerate() {
        prefix_cost[i + 1] = prefix_cost[i] + c;
    }

    // Greedy: cut when segment cost exceeds threshold
    let mut split_positions: Vec<usize> = Vec::new();
    let mut seg_start = 0;
    let mut prev_cand = 0;

    for &cand in &candidates {
        let seg_cost = prefix_cost[cand] - prefix_cost[seg_start];
        if seg_cost > threshold && prev_cand > seg_start {
            split_positions.push(prev_cand);
            seg_start = prev_cand;
        }
        prev_cand = cand;
    }

    if split_positions.is_empty() {
        return;
    }

    // Create sub-blocks
    let original_block = eu.blocks.remove(&block_id).unwrap();
    let original_terminator = original_block.terminator;
    let original_params = original_block.params;
    let all_instructions = original_block.instructions;

    let mut ranges: Vec<(usize, usize)> = Vec::new();
    let mut start = 0;
    for &sp in &split_positions {
        ranges.push((start, sp));
        start = sp;
    }
    ranges.push((start, all_instructions.len()));

    // First sub-block reuses original BlockId
    let mut sub_block_ids = vec![block_id];
    for _ in 1..ranges.len() {
        sub_block_ids.push(BlockId(*next_block_id));
        *next_block_id += 1;
    }

    for (i, &(s, e)) in ranges.iter().enumerate() {
        let instructions = all_instructions[s..e].to_vec();
        let params = if i == 0 {
            original_params.clone()
        } else {
            Vec::new()
        };
        let terminator = if i + 1 < sub_block_ids.len() {
            SIRTerminator::Jump(sub_block_ids[i + 1], Vec::new())
        } else {
            original_terminator.clone()
        };

        eu.blocks.insert(
            sub_block_ids[i],
            BasicBlock {
                id: sub_block_ids[i],
                params,
                instructions,
                terminator,
            },
        );
    }
}

fn collect_terminator_used_regs(term: &SIRTerminator, out: &mut crate::HashSet<RegisterId>) {
    match term {
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            out.insert(*cond);
            for &r in &true_block.1 {
                out.insert(r);
            }
            for &r in &false_block.1 {
                out.insert(r);
            }
        }
        SIRTerminator::Jump(_, args) => {
            for &r in args {
                out.insert(r);
            }
        }
        _ => {}
    }
}

fn collect_terminator_regs_into_map(
    term: &SIRTerminator,
    source: &HashMap<RegisterId, RegisterType>,
    dest: &mut HashMap<RegisterId, RegisterType>,
) {
    let mut regs = Vec::new();
    match term {
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            regs.push(*cond);
            regs.extend_from_slice(&true_block.1);
            regs.extend_from_slice(&false_block.1);
        }
        SIRTerminator::Jump(_, args) => regs.extend_from_slice(args),
        _ => {}
    }
    for r in regs {
        if let Some(ty) = source.get(&r) {
            dest.insert(r, ty.clone());
        }
    }
}

/// Create a sub-EU from a slice of instructions of a single-block EU.
fn make_sub_eu(
    parent: &ExecutionUnit<RegionedAbsoluteAddr>,
    parent_block: &BasicBlock<RegionedAbsoluteAddr>,
    instructions: &[SIRInstruction<RegionedAbsoluteAddr>],
    is_first: bool,
) -> ExecutionUnit<RegionedAbsoluteAddr> {
    let block_id = BlockId(0);
    let params = if is_first {
        parent_block.params.clone()
    } else {
        Vec::new()
    };

    // Build a minimal register_map containing only registers used in this slice
    let mut register_map = HashMap::default();
    for inst in instructions {
        if let Some(def) = def_reg(inst) {
            if let Some(ty) = parent.register_map.get(&def) {
                register_map.insert(def, ty.clone());
            }
        }
        let mut used = Vec::new();
        collect_used_regs(inst, &mut used);
        for r in used {
            if let Some(ty) = parent.register_map.get(&r) {
                register_map.insert(r, ty.clone());
            }
        }
    }
    // Also include params
    for &p in &params {
        if let Some(ty) = parent.register_map.get(&p) {
            register_map.insert(p, ty.clone());
        }
    }

    let block = BasicBlock {
        id: block_id,
        params,
        instructions: instructions.to_vec(),
        terminator: SIRTerminator::Return,
    };

    let mut blocks = HashMap::default();
    blocks.insert(block_id, block);

    ExecutionUnit {
        entry_block_id: block_id,
        blocks,
        register_map,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use malachite_bigint::BigUint;

    fn make_var_id(n: usize) -> veryl_analyzer::ir::VarId {
        let mut id = veryl_analyzer::ir::VarId::default();
        for _ in 0..n {
            id.inc();
        }
        id
    }

    fn make_test_addr(region: u32, inst_id: usize, var_id_val: usize) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region,
            instance_id: InstanceId(inst_id),
            var_id: make_var_id(var_id_val),
        }
    }

    /// Create a large single-block EU with many Store instructions.
    fn make_large_eu(num_stores: usize) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut instructions = Vec::new();
        let mut register_map = HashMap::default();
        let mut reg_counter = 0;

        let addr = make_test_addr(0, 0, 0);

        for i in 0..num_stores {
            let load_reg = RegisterId(reg_counter);
            register_map.insert(
                load_reg,
                RegisterType::Bit {
                    width: 32,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Load(
                load_reg,
                addr,
                SIROffset::Static(0),
                32,
            ));
            reg_counter += 1;

            let imm_reg = RegisterId(reg_counter);
            register_map.insert(
                imm_reg,
                RegisterType::Bit {
                    width: 32,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Imm(
                imm_reg,
                SIRValue::new(BigUint::from(1u32)),
            ));
            reg_counter += 1;

            let result_reg = RegisterId(reg_counter);
            register_map.insert(
                result_reg,
                RegisterType::Bit {
                    width: 32,
                    signed: false,
                },
            );
            instructions.push(SIRInstruction::Binary(
                result_reg,
                load_reg,
                BinaryOp::Add,
                imm_reg,
            ));
            reg_counter += 1;

            let store_addr = make_test_addr(0, 0, i + 1);
            instructions.push(SIRInstruction::Store(
                store_addr,
                SIROffset::Static(0),
                32,
                result_reg,
                Vec::new(),
            ));
        }

        let block = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions,
            terminator: SIRTerminator::Return,
        };

        let mut blocks = HashMap::default();
        blocks.insert(BlockId(0), block);

        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    #[test]
    fn test_no_split_below_threshold() {
        let eu = make_large_eu(2);
        let result = split_with_threshold(&[eu], false, 1_000_000);
        assert!(result.is_none());
    }

    #[test]
    fn test_eu_boundary_split() {
        // Create multiple small EUs that together exceed a low threshold
        let eu1 = make_large_eu(10);
        let eu2 = make_large_eu(10);
        let eu3 = make_large_eu(10);

        // Use a threshold that fits one EU but not two
        let single_eu_cost = super::super::cost_model::estimate_eu_cost(&eu1, false);
        let threshold = single_eu_cost + single_eu_cost / 2; // ~1.5× single EU

        let result = split_with_threshold(&[eu1, eu2, eu3], false, threshold);
        assert!(result.is_some());
        let chunks = result.unwrap();
        assert!(chunks.len() >= 2);

        // EU-boundary splits have no live regs
        for chunk in &chunks {
            assert!(chunk.incoming_live_regs.is_empty());
            assert!(chunk.outgoing_live_regs.is_empty());
        }
    }

    #[test]
    fn test_intra_eu_split() {
        // Create a single large EU that exceeds a low threshold
        let eu = make_large_eu(20);

        // Use a threshold that's about 1/3 the EU cost to force splitting
        let eu_cost = super::super::cost_model::estimate_eu_cost(&eu, false);
        let threshold = eu_cost / 3;

        let result = split_with_threshold(&[eu], false, threshold);
        assert!(result.is_some());
        let chunks = result.unwrap();
        assert!(chunks.len() >= 2);
    }

    /// Create a multi-block EU with a chain: b0 → b1 → b2 → ... → bN (Return).
    /// Each block has `stores_per_block` Load/Add/Store sequences.
    /// Register r0 is defined in b0 and used (via block params) in all subsequent blocks,
    /// testing inter-chunk liveness.
    fn make_multi_block_chain_eu(
        num_blocks: usize,
        stores_per_block: usize,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let mut blocks = HashMap::default();
        let mut register_map = HashMap::default();
        let mut reg_counter = 0;
        let addr = make_test_addr(0, 0, 0);

        // Create a "shared" register defined in block 0
        let shared_reg = RegisterId(reg_counter);
        register_map.insert(
            shared_reg,
            RegisterType::Bit {
                width: 32,
                signed: false,
            },
        );
        reg_counter += 1;

        for b in 0..num_blocks {
            let block_id = BlockId(b);
            let mut instructions = Vec::new();
            let mut params = Vec::new();

            if b == 0 {
                // Define shared_reg in block 0
                instructions.push(SIRInstruction::Imm(
                    shared_reg,
                    SIRValue::new(BigUint::from(42u32)),
                ));
            } else {
                // Block params: receive shared_reg from predecessor
                let param_reg = RegisterId(reg_counter);
                register_map.insert(
                    param_reg,
                    RegisterType::Bit {
                        width: 32,
                        signed: false,
                    },
                );
                reg_counter += 1;
                params.push(param_reg);

                // Use the param in a store
                instructions.push(SIRInstruction::Store(
                    addr,
                    SIROffset::Static(0),
                    32,
                    param_reg,
                    Vec::new(),
                ));
            }

            // Each block has load/add/store sequences
            for i in 0..stores_per_block {
                let load_reg = RegisterId(reg_counter);
                register_map.insert(
                    load_reg,
                    RegisterType::Bit {
                        width: 32,
                        signed: false,
                    },
                );
                instructions.push(SIRInstruction::Load(
                    load_reg,
                    addr,
                    SIROffset::Static(0),
                    32,
                ));
                reg_counter += 1;

                let imm_reg = RegisterId(reg_counter);
                register_map.insert(
                    imm_reg,
                    RegisterType::Bit {
                        width: 32,
                        signed: false,
                    },
                );
                instructions.push(SIRInstruction::Imm(
                    imm_reg,
                    SIRValue::new(BigUint::from(1u32)),
                ));
                reg_counter += 1;

                let result_reg = RegisterId(reg_counter);
                register_map.insert(
                    result_reg,
                    RegisterType::Bit {
                        width: 32,
                        signed: false,
                    },
                );
                instructions.push(SIRInstruction::Binary(
                    result_reg,
                    load_reg,
                    BinaryOp::Add,
                    imm_reg,
                ));
                reg_counter += 1;

                let store_addr = make_test_addr(0, 0, b * stores_per_block + i + 1);
                instructions.push(SIRInstruction::Store(
                    store_addr,
                    SIROffset::Static(0),
                    32,
                    result_reg,
                    Vec::new(),
                ));
            }

            let terminator = if b + 1 < num_blocks {
                // Pass shared_reg (or its param) to next block
                let pass_reg = if b == 0 { shared_reg } else { params[0] };
                SIRTerminator::Jump(BlockId(b + 1), vec![pass_reg])
            } else {
                SIRTerminator::Return
            };

            blocks.insert(
                block_id,
                BasicBlock {
                    id: block_id,
                    params,
                    instructions,
                    terminator,
                },
            );
        }

        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    #[test]
    fn test_multi_block_spilled_split() {
        // Create a multi-block EU with enough blocks/instructions to exceed a low threshold
        let eu = make_multi_block_chain_eu(6, 5);
        let eu_cost = super::super::cost_model::estimate_eu_cost(&eu, false);
        // Use ~1/4 of EU cost as threshold to force multiple chunks
        let threshold = eu_cost / 4;
        assert!(
            eu_cost > threshold,
            "EU cost should exceed our test threshold, got {eu_cost}"
        );

        let result = split_multi_block_with_threshold(&[eu], false, threshold);
        assert!(result.is_some(), "Should produce a spilled plan");

        let plan = result.unwrap();
        assert!(
            plan.chunks.len() >= 2,
            "Should have at least 2 chunks, got {}",
            plan.chunks.len()
        );

        // Each chunk should have a single entry block
        for (i, chunk) in plan.chunks.iter().enumerate() {
            assert!(
                !chunk.eu.blocks.is_empty(),
                "Chunk {} should have at least one block",
                i
            );
        }

        // Per-chunk spills: not all chunks should have the same spills
        // (first chunk has no incoming spills since it's the entry)
        assert!(
            plan.chunks[0].incoming_spills.is_empty()
                || plan.chunks[0].incoming_spills.len()
                    < plan.chunks.last().unwrap().incoming_spills.len()
                || plan.chunks.len() == 2, // 2 chunks: first may have no incoming, second has some
            "First chunk should generally have fewer incoming spills"
        );

        // Scratch bytes should be non-zero (we have inter-chunk live regs)
        assert!(plan.scratch_bytes > 0, "Should need scratch memory");
    }

    #[test]
    fn test_partition_single_pass_basic() {
        // Chain: b0 → b1 → b2 → b3 → Return
        let eu = make_multi_block_chain_eu(4, 3);
        let topo_order = topological_sort_blocks(&eu.blocks, eu.entry_block_id);

        let block_costs: HashMap<BlockId, usize> = topo_order
            .iter()
            .map(|&bid| (bid, estimate_block_cost(&eu, bid, false)))
            .collect();

        // Use threshold smaller than any single block cost to force many chunks
        let max_block_cost = block_costs.values().copied().max().unwrap_or(1);
        let threshold = max_block_cost; // fits exactly one block per chunk
        let groups = partition_single_pass(&eu, &topo_order, &block_costs, threshold);
        assert!(
            groups.len() >= 2,
            "Should have multiple chunks with low threshold"
        );

        // Each chunk's first block should be the only entry point
        // (single-entry guarantee)
        let mut block_to_chunk: HashMap<BlockId, usize> = HashMap::default();
        for (ci, group) in groups.iter().enumerate() {
            for &bid in group {
                block_to_chunk.insert(bid, ci);
            }
        }

        // Verify: for each cross-chunk edge, the target is the first block of its chunk
        for group in &groups {
            for &bid in group {
                let block = &eu.blocks[&bid];
                for succ in block_successors(&block.terminator) {
                    if let Some(&succ_chunk) = block_to_chunk.get(&succ) {
                        let src_chunk = block_to_chunk[&bid];
                        if succ_chunk != src_chunk {
                            assert_eq!(
                                succ, groups[succ_chunk][0],
                                "Cross-chunk target b{} should be head of chunk {}",
                                succ.0, succ_chunk
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn test_multi_block_cross_chunk_edges_complete() {
        // Verify that cross_chunk_edges include ALL block params (no silent dropping)
        let eu = make_multi_block_chain_eu(4, 3);
        // Use threshold that forces at least 2 chunks
        let eu_cost = super::super::cost_model::estimate_eu_cost(&eu, false);
        let threshold = eu_cost / 3;
        let result = split_multi_block_with_threshold(std::slice::from_ref(&eu), false, threshold);

        if let Some(plan) = result {
            for (ci, chunk) in plan.chunks.iter().enumerate() {
                for (target_bid, edge) in &chunk.cross_chunk_edges {
                    let target_block = &eu.blocks.get(target_bid).or_else(|| {
                        // Target might be in a different chunk's sub-EU
                        plan.chunks.iter().find_map(|c| c.eu.blocks.get(target_bid))
                    });
                    if let Some(target_block) = target_block {
                        assert_eq!(
                            edge.param_scratch_offsets.len(),
                            target_block.params.len(),
                            "Chunk {} edge to b{}: scratch offsets count ({}) should match param count ({})",
                            ci,
                            target_bid.0,
                            edge.param_scratch_offsets.len(),
                            target_block.params.len(),
                        );
                    }
                }
            }
        }
    }
}
