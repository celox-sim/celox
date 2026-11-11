//! Recover a packed dynamic lane store from an analyzer-expanded static ladder.
//!
//! A common HDL workaround spells `packed[index] = value` as one static
//! equality and Mux per lane. Once those lanes feed only a contiguous group of
//! equivalent Stores, retaining the value-level ladder is unnecessary: copy
//! the packed base and perform one guarded dynamic lane update at every sink.

use super::cost_model::estimate_clif_cost;
use super::pass_manager::ExecutionUnitPass;
use super::shared::def_reg;
use crate::PassOptions;
use crate::ir::*;
use crate::{HashMap, HashSet, OptimizationContext};
use num_bigint::BigUint;
use num_traits::{One, ToPrimitive, Zero};

#[derive(Default)]
pub(in crate::optimizer) struct PackedScatterStorePass {
    unpacked_element_widths: HashMap<AbsoluteAddr, usize>,
}

impl PackedScatterStorePass {
    pub(in crate::optimizer) fn for_program(program: &OptimizationContext) -> Self {
        let mut unpacked_element_widths = HashMap::default();
        for (&address, info) in &program.design.state_objects {
            if info.array_dims.is_empty() {
                continue;
            }
            let Some(element_count) = info
                .array_dims
                .iter()
                .try_fold(1usize, |count, &dimension| count.checked_mul(dimension))
            else {
                continue;
            };
            if element_count == 0 || info.width % element_count != 0 {
                continue;
            }
            unpacked_element_widths.insert(address, info.width / element_count);
        }
        Self {
            unpacked_element_widths,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct DefSite {
    block: BlockId,
    index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BitSlice {
    root: RegisterId,
    offset: usize,
}

#[derive(Clone, Debug)]
struct ExactEquality {
    selector: RegisterId,
    key: BigUint,
}

#[derive(Clone, Copy, Debug)]
struct LaneMatch {
    gate: Option<RegisterId>,
    selector: RegisterId,
    key: u64,
    value: RegisterId,
    base: BitSlice,
    outer_mux: RegisterId,
    inner_mux: RegisterId,
}

#[derive(Clone)]
struct Rewrite {
    registers: Vec<(RegisterId, RegisterType)>,
    head: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    update: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    condition: Option<RegisterId>,
    static_updates: Vec<StaticUpdate>,
}

#[derive(Clone)]
struct StaticUpdate {
    condition: RegisterId,
    stores: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
}

#[derive(Clone)]
struct GeneratedBlocks {
    dynamic_update: BlockId,
    suffix: BlockId,
    static_updates: Vec<(BlockId, BlockId)>,
}

#[derive(Clone)]
struct ScatterPlan {
    block: BlockId,
    store_index: usize,
    store_count: usize,
    dead: HashSet<DefSite>,
    rewrite: Rewrite,
    new_blocks: Option<GeneratedBlocks>,
    benefit: u128,
}

impl ExecutionUnitPass for PackedScatterStorePass {
    fn name(&self) -> &'static str {
        "packed_scatter_store"
    }

    fn handles_calls(&self) -> bool {
        true
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        // An X/Z selector makes every procedural equality false after
        // ToTwoState. A raw dynamic offset cannot express that without an
        // additional knownness proof, so this transform is two-state only.
        if options.four_state || eu.verify_result().is_err() {
            return;
        }

        // Applying a rewrite can make another Concat observable only through
        // equivalent Stores after the old closed DAG is swept. Alternate
        // discovery and DCE to a structural fixed point. Every rewrite removes
        // the current Concat Store source; if the copied base is itself a
        // Concat, it is a strict SSA ancestor of that source, so repeated
        // recovery still makes structural progress without an iteration
        // budget.
        let mut pending_dce = false;
        loop {
            if let Some(plan) = find_best_plan(eu, &self.unpacked_element_widths) {
                apply_plan(eu, plan);
                pending_dce = true;
                continue;
            }
            if pending_dce {
                prune_dead_pure_instructions(eu);
                pending_dce = false;
                continue;
            }
            break;
        }
    }
}

fn find_best_plan(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
) -> Option<ScatterPlan> {
    let defs = definition_sites(eu);
    let uses = use_counts(eu);
    let mut blocks = eu.blocks.keys().copied().collect::<Vec<_>>();
    blocks.sort_unstable_by_key(|block| block.0);

    let mut best = None;
    for block_id in blocks {
        let block = &eu.blocks[&block_id];
        for store_index in 0..block.instructions.len() {
            let Some(plan) = plan_store(
                eu,
                block,
                store_index,
                &defs,
                &uses,
                unpacked_element_widths,
            ) else {
                continue;
            };
            let replace = best.as_ref().is_none_or(|current: &ScatterPlan| {
                plan.benefit > current.benefit
                    || (plan.benefit == current.benefit
                        && (plan.block.0, plan.store_index)
                            < (current.block.0, current.store_index))
            });
            if replace {
                best = Some(plan);
            }
        }
    }
    best
}

fn plan_store(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    block: &BasicBlock<RegionedAbsoluteAddr>,
    store_index: usize,
    defs: &HashMap<RegisterId, DefSite>,
    uses: &HashMap<RegisterId, usize>,
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
) -> Option<ScatterPlan> {
    let SIRInstruction::Store(
        _,
        SIROffset::Static(destination_start),
        width,
        packed,
        triggers,
        capture_sites,
    ) = block.instructions.get(store_index)?
    else {
        return None;
    };
    if !triggers.is_empty() || !capture_sites.is_empty() || *width == 0 {
        return None;
    }
    destination_start.checked_add(*width)?;
    // Every observable use must be one member of a single contiguous group of
    // identical stores. The value-level ladder can then be replaced at all of
    // its sinks without materializing the packed value. Matching the use count
    // also rejects an instruction or CFG edge that observes the source
    // elsewhere.
    let store_count = uses.get(packed).copied()?;
    let store_end = store_index.checked_add(store_count)?;
    let store_group = block.instructions.get(store_index..store_end)?;
    let mut addresses = Vec::with_capacity(store_count);
    for instruction in store_group {
        let SIRInstruction::Store(
            address,
            SIROffset::Static(candidate_start),
            candidate_width,
            candidate_packed,
            candidate_triggers,
            candidate_capture_sites,
        ) = instruction
        else {
            return None;
        };
        if candidate_start != destination_start
            || candidate_width != width
            || candidate_packed != packed
            || !candidate_triggers.is_empty()
            || !candidate_capture_sites.is_empty()
        {
            return None;
        }
        addresses.push(*address);
    }
    let concat_site = *defs.get(packed)?;
    if concat_site.block != block.id || concat_site.index >= store_index {
        return None;
    }
    let SIRInstruction::Concat(_, lanes) = instruction_at(eu, concat_site)? else {
        return None;
    };
    if lanes.len() < 2 {
        return None;
    }
    let lane_width = eu.register_map.get(lanes.first()?)?.width();
    if lane_width == 0
        || lanes
            .iter()
            .any(|lane| eu.register_map.get(lane).map(RegisterType::width) != Some(lane_width))
        || lane_width.checked_mul(lanes.len())? != *width
    {
        return None;
    }

    let mut matches = Vec::with_capacity(lanes.len());
    for lane in lanes.iter().rev().copied() {
        matches.push(match_lane(eu, defs, lane, lane_width)?);
    }
    let first = *matches.first()?;
    let selector_type = eu.register_map.get(&first.selector)?;
    let selector_width = selector_type.width();
    if selector_width == 0 || selector_width > 64 || selector_type.is_signed() {
        return None;
    }
    let maximum_key = if selector_width == 64 {
        u64::MAX
    } else {
        (1u64 << selector_width) - 1
    };
    let last_key = first
        .key
        .checked_add(u64::try_from(matches.len().checked_sub(1)?).ok()?)?;
    if last_key > maximum_key {
        return None;
    }

    let base_start = first.base.offset;
    let base_root = first.base.root;
    let gate = first.gate;
    let value = first.value;
    for (lane, matched) in matches.iter().enumerate() {
        let lane_offset = lane_width.checked_mul(lane)?;
        if matched.gate != gate
            || matched.selector != first.selector
            || matched.value != value
            || matched.key != first.key.checked_add(u64::try_from(lane).ok()?)?
            || matched.base.root != base_root
            || matched.base.offset != base_start.checked_add(lane_offset)?
        {
            return None;
        }
    }
    if eu.register_map.get(&value)?.width() != lane_width
        || base_start.checked_add(*width)? > eu.register_map.get(&base_root)?.width()
    {
        return None;
    }

    // An unpacked array is not a packed bit vector in SystemVerilog. Recover
    // a dynamic update only when the matched lanes are exactly its declared
    // elements; then the selector is an element index. A smaller lane would
    // flatten the unpacked/packed boundary and a larger lane would span
    // elements, neither of which SIROffset::Dynamic is allowed to express.
    let array_element_width = addresses
        .iter()
        .filter_map(|address| {
            unpacked_element_widths
                .get(&address.absolute_addr())
                .copied()
        })
        .next();
    if let Some(element_width) = array_element_width
        && (element_width != lane_width
            || destination_start % element_width != 0
            || addresses.iter().any(|address| {
                unpacked_element_widths
                    .get(&address.absolute_addr())
                    .is_some_and(|&candidate| candidate != element_width)
            }))
    {
        return None;
    }
    if let Some(gate) = gate
        && !matches!(
            eu.register_map.get(&gate),
            Some(RegisterType::Bit {
                width: 1,
                signed: false
            })
        )
    {
        return None;
    }

    // Every backend currently lowers a dynamic bit offset without carrying an
    // alignment proof, so containment must use the worst possible intra-byte
    // shift even when selector * lane_width is mathematically byte aligned.
    // A scalar of at most 57 bits fits in one 64-bit access at every such
    // shift. Wider dynamic lanes would require a second machine word.
    if lane_width > 57 {
        return None;
    }
    let destination_end = destination_start.checked_add(*width)?.div_ceil(8);
    let dynamic_bytes = native_access_bytes(lane_width.checked_add(7)?)?;
    let dynamic_lane_is_contained = |lane: usize| {
        dynamic_lane_access_is_contained(
            *destination_start,
            lane_width,
            lane,
            dynamic_bytes,
            destination_end,
        )
    };
    let dynamic_lanes = (0..matches.len())
        .take_while(|&lane| dynamic_lane_is_contained(lane))
        .count();
    if dynamic_lanes == 0 || (dynamic_lanes..matches.len()).any(dynamic_lane_is_contained) {
        return None;
    }
    let dynamic_last_key = first
        .key
        .checked_add(u64::try_from(dynamic_lanes.checked_sub(1)?).ok()?)?;
    let peeled_keys = matches[dynamic_lanes..]
        .iter()
        .map(|lane| lane.key)
        .collect::<Vec<_>>();
    for &key in &peeled_keys {
        let lane = usize::try_from(key.checked_sub(first.key)?).ok()?;
        let bit_offset = destination_start.checked_add(lane_width.checked_mul(lane)?)?;
        let intra = bit_offset % 8;
        let bytes = native_access_bytes(lane_width.checked_add(intra)?)?;
        if bit_offset
            .div_euclid(8)
            .checked_add(bytes)
            .is_none_or(|end| end > destination_end)
        {
            return None;
        }
    }

    let protected = matches
        .iter()
        .flat_map(|lane| {
            [
                lane.selector,
                lane.value,
                lane.base.root,
                lane.gate.unwrap_or(lane.selector),
            ]
        })
        .collect::<HashSet<_>>();
    let dead = dead_after_removing_store_sources(eu, defs, uses, *packed, store_count, &protected)?;
    if !dead.contains(&concat_site) {
        return None;
    }
    for lane in &matches {
        let outer = *defs.get(&lane.outer_mux)?;
        let inner = *defs.get(&lane.inner_mux)?;
        if !dead.contains(&outer) || !dead.contains(&inner) {
            return None;
        }
    }

    let rewrite = build_rewrite(
        eu,
        &addresses,
        *destination_start,
        *width,
        lane_width,
        base_root,
        base_start,
        first.selector,
        first.key,
        dynamic_last_key,
        maximum_key,
        value,
        gate,
        &peeled_keys,
        unpacked_element_widths,
    )?;
    let new_blocks = if rewrite.condition.is_some() {
        let max_block = eu.blocks.keys().map(|block| block.0).max().unwrap_or(0);
        let dynamic_update = max_block.checked_add(1)?;
        let suffix = max_block.checked_add(2)?;
        let generated = 2usize.checked_add(rewrite.static_updates.len().checked_mul(2)?)?;
        let last = max_block.checked_add(generated)?;
        if last > u32::MAX as usize {
            return None;
        }
        let static_updates = (0..rewrite.static_updates.len())
            .map(|index| {
                let decision = max_block.checked_add(3 + index * 2)?;
                let update = max_block.checked_add(4 + index * 2)?;
                Some((BlockId(decision), BlockId(update)))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(GeneratedBlocks {
            dynamic_update: BlockId(dynamic_update),
            suffix: BlockId(suffix),
            static_updates,
        })
    } else {
        None
    };
    let mut cost_registers = eu.register_map.clone();
    cost_registers.extend(rewrite.registers.iter().cloned());
    let removed_store_cost = store_group
        .iter()
        .map(|instruction| estimate_clif_cost(instruction, &eu.register_map, false) as u128)
        .fold(0u128, u128::saturating_add);
    let removed_cost = dead
        .iter()
        .map(|site| {
            estimate_clif_cost(instruction_at(eu, *site).unwrap(), &eu.register_map, false) as u128
        })
        .fold(removed_store_cost, u128::saturating_add);
    let instruction_cost = rewrite
        .head
        .iter()
        .chain(rewrite.update.iter())
        .chain(
            rewrite
                .static_updates
                .iter()
                .flat_map(|update| update.stores.iter()),
        )
        .map(|instruction| estimate_clif_cost(instruction, &cost_registers, false) as u128)
        .fold(0u128, u128::saturating_add);
    let control_cost = if rewrite.condition.is_some() {
        // Each selected update has one Branch, one Jump and a conservative
        // one-time miss cost. Peeled static lanes add one exact decision.
        (3u128 + 16u128).saturating_mul(1 + rewrite.static_updates.len() as u128)
    } else {
        0
    };
    let introduced_cost = instruction_cost.saturating_add(control_cost);
    if removed_cost <= introduced_cost {
        return None;
    }

    Some(ScatterPlan {
        block: block.id,
        store_index,
        store_count,
        dead,
        rewrite,
        new_blocks,
        benefit: removed_cost - introduced_cost,
    })
}

fn native_access_bytes(bits: usize) -> Option<usize> {
    match bits {
        1..=8 => Some(1),
        9..=16 => Some(2),
        17..=32 => Some(4),
        33..=64 => Some(8),
        _ => None,
    }
}

fn dynamic_lane_access_is_contained(
    destination_start: usize,
    lane_width: usize,
    lane: usize,
    dynamic_bytes: usize,
    destination_end: usize,
) -> bool {
    lane_width
        .checked_mul(lane)
        .and_then(|offset| destination_start.checked_add(offset))
        .and_then(|bit_offset| bit_offset.div_euclid(8).checked_add(dynamic_bytes))
        .is_some_and(|end| end <= destination_end)
}

fn match_lane(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    defs: &HashMap<RegisterId, DefSite>,
    lane: RegisterId,
    lane_width: usize,
) -> Option<LaneMatch> {
    let SIRInstruction::Mux(outer_dst, outer_cond, outer_true, outer_false) =
        defining_instruction(eu, defs, lane)?
    else {
        return None;
    };

    // First try the guarded form emitted for `if enable { if index == K }`.
    if let Some(SIRInstruction::Mux(inner_dst, inner_cond, value, inner_base)) =
        defining_instruction(eu, defs, *outer_true)
        && let Some(equality) = match_exact_equality(eu, defs, *inner_cond)
        && let Some(inner_slice) = resolve_low_slice(eu, defs, *inner_base, lane_width)
        && let Some(outer_slice) = resolve_low_slice(eu, defs, *outer_false, lane_width)
        && inner_slice == outer_slice
        && eu.register_map.get(value).map(RegisterType::width) == Some(lane_width)
    {
        return Some(LaneMatch {
            gate: Some(*outer_cond),
            selector: equality.selector,
            key: equality.key.to_u64()?,
            value: *value,
            base: inner_slice,
            outer_mux: *outer_dst,
            inner_mux: *inner_dst,
        });
    }

    // Unguarded lane update: Mux(index == K, value, base).
    let equality = match_exact_equality(eu, defs, *outer_cond)?;
    let base = resolve_low_slice(eu, defs, *outer_false, lane_width)?;
    if eu.register_map.get(outer_true).map(RegisterType::width) != Some(lane_width) {
        return None;
    }
    Some(LaneMatch {
        gate: None,
        selector: equality.selector,
        key: equality.key.to_u64()?,
        value: *outer_true,
        base,
        outer_mux: *outer_dst,
        inner_mux: *outer_dst,
    })
}

fn match_exact_equality(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    defs: &HashMap<RegisterId, DefSite>,
    condition: RegisterId,
) -> Option<ExactEquality> {
    let mut cursor = condition;
    let mut seen = HashSet::default();
    while seen.insert(cursor) {
        match defining_instruction(eu, defs, cursor)? {
            SIRInstruction::Unary(dst, UnaryOp::ToTwoState | UnaryOp::Ident, inner)
                if eu.register_map.get(dst)?.width() == 1
                    && eu.register_map.get(inner)?.width() == 1 =>
            {
                cursor = *inner;
            }
            SIRInstruction::Unary(dst, UnaryOp::Or, inner)
                if eu.register_map.get(dst)?.width() == 1
                    && eu.register_map.get(inner)?.width() == 1 =>
            {
                cursor = *inner;
            }
            _ => break,
        }
    }
    let SIRInstruction::Binary(result, lhs, BinaryOp::Eq, rhs) =
        defining_instruction(eu, defs, cursor)?
    else {
        return None;
    };
    if eu.register_map.get(result)?.width() != 1 {
        return None;
    }
    let lhs_constant = exact_constant(eu, defs, *lhs);
    let rhs_constant = exact_constant(eu, defs, *rhs);
    let (selector, key_reg, key) = match (lhs_constant, rhs_constant) {
        (None, Some(key)) => (*lhs, *rhs, key),
        (Some(key), None) => (*rhs, *lhs, key),
        _ => return None,
    };
    let compare_width = eu.register_map.get(&selector)?.width();
    if compare_width == 0 || eu.register_map.get(&key_reg)?.width() != compare_width {
        return None;
    }
    let selector = canonical_selector(eu, defs, selector);
    let selector_width = eu.register_map.get(&selector)?.width();
    let key = truncate(key, compare_width);
    if selector_width == 0 || !fits_width(&key, selector_width) {
        return None;
    }
    Some(ExactEquality { selector, key })
}

fn canonical_selector(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    defs: &HashMap<RegisterId, DefSite>,
    mut register: RegisterId,
) -> RegisterId {
    let mut seen = HashSet::default();
    while seen.insert(register) {
        match defining_instruction(eu, defs, register) {
            Some(SIRInstruction::Unary(dst, UnaryOp::Ident, inner))
                if identity_preserves_low_bits(eu, *dst, *inner) =>
            {
                register = *inner;
            }
            Some(SIRInstruction::Concat(dst, parts)) if !parts.is_empty() => {
                let Some((&low, high)) = parts.split_last() else {
                    break;
                };
                let Some(low_width) = eu.register_map.get(&low).map(RegisterType::width) else {
                    break;
                };
                let Some(total) = high.iter().try_fold(low_width, |sum, part| {
                    sum.checked_add(eu.register_map.get(part)?.width())
                }) else {
                    break;
                };
                if eu.register_map.get(dst).map(RegisterType::width) != Some(total)
                    || high.iter().any(|part| {
                        exact_constant(eu, defs, *part).is_none_or(|value| !value.is_zero())
                    })
                {
                    break;
                }
                register = low;
            }
            Some(SIRInstruction::Slice(dst, source, 0, width))
                if *width
                    == eu
                        .register_map
                        .get(source)
                        .map(RegisterType::width)
                        .unwrap_or(0)
                    && identity_preserves_low_bits(eu, *dst, *source) =>
            {
                register = *source;
            }
            _ => break,
        }
    }
    register
}

fn identity_preserves_low_bits(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    destination: RegisterId,
    source: RegisterId,
) -> bool {
    let Some(destination) = eu.register_map.get(&destination) else {
        return false;
    };
    let Some(source) = eu.register_map.get(&source) else {
        return false;
    };
    source.width() != 0 && destination.width() >= source.width() && !source.is_signed()
}

fn resolve_low_slice(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    defs: &HashMap<RegisterId, DefSite>,
    register: RegisterId,
    width: usize,
) -> Option<BitSlice> {
    let mut cursor = register;
    let mut offset = 0usize;
    let mut seen = HashSet::default();
    while seen.insert(cursor) {
        if offset.checked_add(width)? > eu.register_map.get(&cursor)?.width() {
            return None;
        }
        let Some(instruction) = defining_instruction(eu, defs, cursor) else {
            break;
        };
        match instruction {
            SIRInstruction::Slice(_, source, slice_offset, slice_width)
                if *slice_width >= width =>
            {
                offset = offset.checked_add(*slice_offset)?;
                cursor = *source;
            }
            SIRInstruction::Unary(_, UnaryOp::Ident | UnaryOp::ToTwoState, source) => {
                cursor = *source;
            }
            SIRInstruction::Binary(_, source, BinaryOp::Shr, amount) => {
                let amount = exact_constant(eu, defs, *amount)?.to_usize()?;
                offset = offset.checked_add(amount)?;
                cursor = *source;
            }
            SIRInstruction::Binary(_, lhs, BinaryOp::And, rhs) => {
                let lhs_constant = exact_constant(eu, defs, *lhs);
                let rhs_constant = exact_constant(eu, defs, *rhs);
                let (source, mask) = match (lhs_constant, rhs_constant) {
                    (None, Some(mask)) => (*lhs, mask),
                    (Some(mask), None) => (*rhs, mask),
                    _ => break,
                };
                let needed = low_mask(width) << offset;
                if (&mask & &needed) != needed {
                    return None;
                }
                cursor = source;
            }
            _ => break,
        }
    }
    (offset.checked_add(width)? <= eu.register_map.get(&cursor)?.width()).then_some(BitSlice {
        root: cursor,
        offset,
    })
}

fn exact_constant(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    defs: &HashMap<RegisterId, DefSite>,
    register: RegisterId,
) -> Option<BigUint> {
    exact_constant_inner(eu, defs, register, &mut HashSet::default())
}

fn exact_constant_inner(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    defs: &HashMap<RegisterId, DefSite>,
    register: RegisterId,
    active: &mut HashSet<RegisterId>,
) -> Option<BigUint> {
    if !active.insert(register) {
        return None;
    }
    let width = eu.register_map.get(&register)?.width();
    let result = match defining_instruction(eu, defs, register)? {
        SIRInstruction::Imm(_, value) if value.mask.is_zero() => {
            Some(truncate(value.payload.clone(), width))
        }
        SIRInstruction::Unary(_, UnaryOp::Ident | UnaryOp::ToTwoState, source) => {
            exact_constant_inner(eu, defs, *source, active).map(|value| truncate(value, width))
        }
        SIRInstruction::Binary(_, lhs, op, rhs) => {
            let lhs = exact_constant_inner(eu, defs, *lhs, active)?;
            let rhs = exact_constant_inner(eu, defs, *rhs, active)?;
            let value = match op {
                BinaryOp::Add => lhs + rhs,
                BinaryOp::Sub => {
                    let modulus = BigUint::one() << width;
                    let lhs = truncate(lhs, width);
                    let rhs = truncate(rhs, width);
                    (lhs + &modulus - rhs) % modulus
                }
                BinaryOp::Mul => lhs * rhs,
                BinaryOp::And => lhs & rhs,
                BinaryOp::Or => lhs | rhs,
                BinaryOp::Xor => lhs ^ rhs,
                BinaryOp::Shl => lhs << rhs.to_usize()?,
                BinaryOp::Shr => lhs >> rhs.to_usize()?,
                _ => return None,
            };
            Some(truncate(value, width))
        }
        SIRInstruction::Slice(_, source, offset, slice_width) => {
            let source = exact_constant_inner(eu, defs, *source, active)?;
            Some(truncate(source >> offset, *slice_width))
        }
        SIRInstruction::Concat(_, parts) => {
            let mut result = BigUint::zero();
            for part in parts {
                let part_width = eu.register_map.get(part)?.width();
                result = (result << part_width)
                    | truncate(exact_constant_inner(eu, defs, *part, active)?, part_width);
            }
            Some(truncate(result, width))
        }
        _ => None,
    };
    active.remove(&register);
    result
}

#[allow(clippy::too_many_arguments)]
fn build_rewrite(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    addresses: &[RegionedAbsoluteAddr],
    destination_start: usize,
    width: usize,
    lane_width: usize,
    base_root: RegisterId,
    base_start: usize,
    selector: RegisterId,
    first_key: u64,
    dynamic_last_key: u64,
    maximum_key: u64,
    value: RegisterId,
    gate: Option<RegisterId>,
    peeled_keys: &[u64],
    unpacked_element_widths: &HashMap<AbsoluteAddr, usize>,
) -> Option<Rewrite> {
    let mut next = eu
        .register_map
        .keys()
        .map(|register| register.0)
        .max()
        .unwrap_or(0);
    let mut registers = Vec::new();
    let mut head = Vec::new();
    let mut alloc = |ty: RegisterType| {
        next = next.checked_add(1)?;
        let register = RegisterId(next);
        registers.push((register, ty));
        Some(register)
    };

    let base_root_width = eu.register_map.get(&base_root)?.width();
    let base = if base_start == 0 && width == base_root_width {
        base_root
    } else {
        let base_type = match eu.register_map.get(&base_root)? {
            RegisterType::Logic { .. } => RegisterType::Logic { width },
            RegisterType::Bit { .. } => RegisterType::Bit {
                width,
                signed: false,
            },
        };
        let base = alloc(base_type)?;
        head.push(SIRInstruction::Slice(base, base_root, base_start, width));
        base
    };

    let selector_width = eu.register_map.get(&selector)?.width();
    let selector64 = if selector_width == 64 {
        selector
    } else {
        let widened = alloc(RegisterType::Bit {
            width: 64,
            signed: false,
        })?;
        head.push(SIRInstruction::Unary(widened, UnaryOp::Ident, selector));
        widened
    };
    let has_scalar_destination = addresses
        .iter()
        .any(|address| !unpacked_element_widths.contains_key(&address.absolute_addr()));
    let has_array_destination = addresses
        .iter()
        .any(|address| unpacked_element_widths.contains_key(&address.absolute_addr()));
    let scaled = if !has_scalar_destination {
        None
    } else if lane_width == 1 {
        Some(selector64)
    } else {
        let scale = alloc(RegisterType::Bit {
            width: 64,
            signed: false,
        })?;
        head.push(SIRInstruction::Imm(
            scale,
            SIRValue::new(u64::try_from(lane_width).ok()?),
        ));
        let scaled = alloc(RegisterType::Bit {
            width: 64,
            signed: false,
        })?;
        head.push(SIRInstruction::Binary(
            scaled,
            selector64,
            BinaryOp::Mul,
            scale,
        ));
        Some(scaled)
    };
    let lane_width_u64 = u64::try_from(lane_width).ok()?;
    let key_origin = first_key.checked_mul(lane_width_u64)?;
    let destination_origin = u64::try_from(destination_start).ok()?;
    let (bias_op, bias) = if destination_origin >= key_origin {
        (BinaryOp::Add, destination_origin - key_origin)
    } else {
        (BinaryOp::Sub, key_origin - destination_origin)
    };
    let offset = if !has_scalar_destination {
        None
    } else if bias == 0 {
        scaled
    } else {
        let base_offset = alloc(RegisterType::Bit {
            width: 64,
            signed: false,
        })?;
        head.push(SIRInstruction::Imm(base_offset, SIRValue::new(bias)));
        let offset = alloc(RegisterType::Bit {
            width: 64,
            signed: false,
        })?;
        head.push(SIRInstruction::Binary(
            offset,
            scaled?,
            bias_op,
            base_offset,
        ));
        Some(offset)
    };
    let element_index = if !has_array_destination {
        None
    } else {
        let destination_element = destination_start.checked_div(lane_width)?;
        let first_key = usize::try_from(first_key).ok()?;
        let (index_op, index_bias) = if destination_element >= first_key {
            (BinaryOp::Add, destination_element - first_key)
        } else {
            (BinaryOp::Sub, first_key - destination_element)
        };
        if index_bias == 0 {
            Some(selector64)
        } else {
            let bias = alloc(RegisterType::Bit {
                width: 64,
                signed: false,
            })?;
            head.push(SIRInstruction::Imm(
                bias,
                SIRValue::new(u64::try_from(index_bias).ok()?),
            ));
            let index = alloc(RegisterType::Bit {
                width: 64,
                signed: false,
            })?;
            head.push(SIRInstruction::Binary(index, selector64, index_op, bias));
            Some(index)
        }
    };

    let selector_constant_type = match eu.register_map.get(&selector)? {
        RegisterType::Logic { .. } => RegisterType::Logic {
            width: selector_width,
        },
        RegisterType::Bit { .. } => RegisterType::Bit {
            width: selector_width,
            signed: false,
        },
    };
    let mut condition = gate;
    if first_key != 0 {
        let key = alloc(selector_constant_type.clone())?;
        head.push(SIRInstruction::Imm(key, SIRValue::new(first_key)));
        let lower = alloc(RegisterType::Bit {
            width: 1,
            signed: false,
        })?;
        head.push(SIRInstruction::Binary(lower, selector, BinaryOp::GeU, key));
        condition = combine_condition(&mut alloc, &mut head, condition, lower)?;
    }
    if dynamic_last_key != maximum_key {
        let key = alloc(selector_constant_type.clone())?;
        head.push(SIRInstruction::Imm(key, SIRValue::new(dynamic_last_key)));
        let upper = alloc(RegisterType::Bit {
            width: 1,
            signed: false,
        })?;
        head.push(SIRInstruction::Binary(upper, selector, BinaryOp::LeU, key));
        condition = combine_condition(&mut alloc, &mut head, condition, upper)?;
    }

    let mut static_updates = Vec::with_capacity(peeled_keys.len());
    for &peeled_key in peeled_keys {
        let key = alloc(selector_constant_type.clone())?;
        head.push(SIRInstruction::Imm(key, SIRValue::new(peeled_key)));
        let equality = alloc(RegisterType::Bit {
            width: 1,
            signed: false,
        })?;
        head.push(SIRInstruction::Binary(
            equality,
            selector,
            BinaryOp::Eq,
            key,
        ));
        let peeled_condition = combine_condition(&mut alloc, &mut head, gate, equality)?;
        let peeled_condition = peeled_condition?;
        let lane = usize::try_from(peeled_key.checked_sub(first_key)?).ok()?;
        let bit_offset = destination_start.checked_add(lane_width.checked_mul(lane)?)?;
        static_updates.push(StaticUpdate {
            condition: peeled_condition,
            stores: addresses
                .iter()
                .map(|address| {
                    SIRInstruction::Store(
                        *address,
                        SIROffset::Static(bit_offset),
                        lane_width,
                        value,
                        Vec::new(),
                        Vec::new(),
                    )
                })
                .collect(),
        });
    }

    // Distinct canonical storage bases are disjoint. Address aliases share a
    // base, and the matched static offset and width are identical, so aliases
    // touch exactly the same range. The original stores are contiguous and
    // carry no trigger/capture observation: stores to distinct bases commute,
    // while aliases (including duplicate addresses) receive identical base and
    // patch values. Retaining address order therefore preserves final memory.
    head.extend(addresses.iter().map(|address| {
        SIRInstruction::Store(
            *address,
            SIROffset::Static(destination_start),
            width,
            base,
            Vec::new(),
            Vec::new(),
        )
    }));
    let dynamic = addresses
        .iter()
        .map(|address| {
            let offset = if let Some(&element_width) =
                unpacked_element_widths.get(&address.absolute_addr())
            {
                let index = element_index?;
                SIROffset::Element {
                    index,
                    element_width,
                    bit_offset: 0,
                    dynamic_bit_offset: None,
                }
            } else {
                SIROffset::Dynamic(offset?)
            };
            Some(SIRInstruction::Store(
                *address,
                offset,
                lane_width,
                value,
                Vec::new(),
                Vec::new(),
            ))
        })
        .collect::<Option<Vec<_>>>()?;
    let update = if condition.is_some() {
        dynamic
    } else {
        head.extend(dynamic);
        Vec::new()
    };
    Some(Rewrite {
        registers,
        head,
        update,
        condition,
        static_updates,
    })
}

fn combine_condition(
    alloc: &mut impl FnMut(RegisterType) -> Option<RegisterId>,
    instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    lhs: Option<RegisterId>,
    rhs: RegisterId,
) -> Option<Option<RegisterId>> {
    let Some(lhs) = lhs else {
        return Some(Some(rhs));
    };
    let combined = alloc(RegisterType::Bit {
        width: 1,
        signed: false,
    })?;
    instructions.push(SIRInstruction::Binary(
        combined,
        lhs,
        BinaryOp::LogicAnd,
        rhs,
    ));
    Some(Some(combined))
}

fn dead_after_removing_store_sources(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    defs: &HashMap<RegisterId, DefSite>,
    uses: &HashMap<RegisterId, usize>,
    source: RegisterId,
    removed_uses: usize,
    protected: &HashSet<RegisterId>,
) -> Option<HashSet<DefSite>> {
    let mut remaining = uses.clone();
    let count = remaining.get_mut(&source)?;
    *count = count.checked_sub(removed_uses)?;
    let mut work = vec![source];
    let mut dead = HashSet::default();
    while let Some(register) = work.pop() {
        if protected.contains(&register) || remaining.get(&register).copied().unwrap_or(0) != 0 {
            continue;
        }
        let Some(&site) = defs.get(&register) else {
            continue;
        };
        let instruction = instruction_at(eu, site)?;
        if !is_removable_pure(instruction) || !dead.insert(site) {
            continue;
        }
        for operand in instruction_uses(instruction) {
            let count = remaining.get_mut(&operand)?;
            *count = count.checked_sub(1)?;
            if *count == 0 {
                work.push(operand);
            }
        }
    }
    Some(dead)
}

fn apply_plan(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, plan: ScatterPlan) {
    let conditional = match (plan.rewrite.condition, plan.new_blocks) {
        (Some(condition), Some(blocks)) => Some((condition, blocks)),
        (None, None) => None,
        _ => return,
    };
    if conditional
        .as_ref()
        .is_some_and(|(_, blocks)| blocks.static_updates.len() != plan.rewrite.static_updates.len())
        || conditional.is_none() && !plan.rewrite.static_updates.is_empty()
    {
        return;
    }
    let original = eu.blocks[&plan.block].clone();
    let suffix_start = plan.store_index + plan.store_count;
    eu.register_map.extend(plan.rewrite.registers);

    if let Some((condition, blocks)) = conditional {
        let first_fallback = blocks
            .static_updates
            .first()
            .map_or(blocks.suffix, |(decision, _)| *decision);
        let mut head = original.instructions[..plan.store_index].to_vec();
        head.extend(plan.rewrite.head);
        eu.blocks.insert(
            plan.block,
            BasicBlock {
                id: plan.block,
                params: original.params,
                instructions: head,
                terminator: SIRTerminator::Branch {
                    cond: condition,
                    true_block: (blocks.dynamic_update, Vec::new()),
                    false_block: (first_fallback, Vec::new()),
                },
            },
        );
        eu.blocks.insert(
            blocks.dynamic_update,
            BasicBlock {
                id: blocks.dynamic_update,
                params: Vec::new(),
                instructions: plan.rewrite.update,
                terminator: SIRTerminator::Jump(blocks.suffix, Vec::new()),
            },
        );

        for (index, (static_update, (decision_id, update_id))) in plan
            .rewrite
            .static_updates
            .into_iter()
            .zip(blocks.static_updates.iter().copied())
            .enumerate()
        {
            let false_block = blocks
                .static_updates
                .get(index + 1)
                .map_or(blocks.suffix, |(decision, _)| *decision);
            eu.blocks.insert(
                decision_id,
                BasicBlock {
                    id: decision_id,
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Branch {
                        cond: static_update.condition,
                        true_block: (update_id, Vec::new()),
                        false_block: (false_block, Vec::new()),
                    },
                },
            );
            eu.blocks.insert(
                update_id,
                BasicBlock {
                    id: update_id,
                    params: Vec::new(),
                    instructions: static_update.stores,
                    terminator: SIRTerminator::Jump(blocks.suffix, Vec::new()),
                },
            );
        }
        eu.blocks.insert(
            blocks.suffix,
            BasicBlock {
                id: blocks.suffix,
                params: Vec::new(),
                instructions: original.instructions[suffix_start..].to_vec(),
                terminator: original.terminator,
            },
        );
    } else {
        let block = eu.blocks.get_mut(&plan.block).unwrap();
        let mut instructions = original.instructions[..plan.store_index].to_vec();
        instructions.extend(plan.rewrite.head);
        instructions.extend_from_slice(&original.instructions[suffix_start..]);
        block.instructions = instructions;
    }

    if tracing::enabled!(tracing::Level::DEBUG) {
        tracing::debug!(
            "[packed-scatter-store] block={} dead={} benefit={}",
            plan.block.0,
            plan.dead.len(),
            plan.benefit
        );
    }
}

fn prune_dead_pure_instructions(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    let defs = definition_sites(eu);
    let mut work = Vec::new();
    for block in eu.blocks.values() {
        work.extend(terminator_uses(&block.terminator));
        for instruction in &block.instructions {
            if !is_removable_pure(instruction) {
                work.extend(instruction_uses(instruction));
            }
        }
    }
    let mut live = HashSet::default();
    while let Some(register) = work.pop() {
        if !live.insert(register) {
            continue;
        }
        if let Some(instruction) = defining_instruction(eu, &defs, register) {
            work.extend(instruction_uses(instruction));
        }
    }
    let mut removed_registers = HashSet::default();
    for block in eu.blocks.values_mut() {
        block.instructions.retain(|instruction| {
            let remove = is_removable_pure(instruction)
                && def_reg(instruction).is_none_or(|register| !live.contains(&register));
            if remove && let Some(register) = def_reg(instruction) {
                removed_registers.insert(register);
            }
            !remove
        });
    }
    eu.register_map
        .retain(|register, _| !removed_registers.contains(register));
}

fn is_removable_pure(instruction: &SIRInstruction<RegionedAbsoluteAddr>) -> bool {
    !instruction.has_side_effects()
}

fn definition_sites(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<RegisterId, DefSite> {
    eu.blocks
        .values()
        .flat_map(|block| {
            block
                .instructions
                .iter()
                .enumerate()
                .filter_map(move |(index, instruction)| {
                    def_reg(instruction).map(|register| {
                        (
                            register,
                            DefSite {
                                block: block.id,
                                index,
                            },
                        )
                    })
                })
        })
        .collect()
}

fn use_counts(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<RegisterId, usize> {
    let mut counts = HashMap::default();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            for register in instruction_uses(instruction) {
                *counts.entry(register).or_default() += 1;
            }
        }
        for register in terminator_uses(&block.terminator) {
            *counts.entry(register).or_default() += 1;
        }
    }
    counts
}

fn instruction_at(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    site: DefSite,
) -> Option<&SIRInstruction<RegionedAbsoluteAddr>> {
    eu.blocks.get(&site.block)?.instructions.get(site.index)
}

fn defining_instruction<'a>(
    eu: &'a ExecutionUnit<RegionedAbsoluteAddr>,
    defs: &HashMap<RegisterId, DefSite>,
    register: RegisterId,
) -> Option<&'a SIRInstruction<RegionedAbsoluteAddr>> {
    instruction_at(eu, *defs.get(&register)?)
}

fn instruction_uses(instruction: &SIRInstruction<RegionedAbsoluteAddr>) -> Vec<RegisterId> {
    let mut uses = Vec::new();
    instruction.for_each_use(|register| uses.push(register));
    uses
}

fn terminator_uses(terminator: &SIRTerminator) -> Vec<RegisterId> {
    match terminator {
        SIRTerminator::Jump(_, args) => args.clone(),
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            let mut result = vec![*cond];
            result.extend(true_block.1.iter().copied());
            result.extend(false_block.1.iter().copied());
            result
        }
        SIRTerminator::Switch { selector, .. } => vec![*selector],
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

fn low_mask(width: usize) -> BigUint {
    (BigUint::one() << width) - BigUint::one()
}

fn truncate(value: BigUint, width: usize) -> BigUint {
    value & low_mask(width)
}

fn fits_width(value: &BigUint, width: usize) -> bool {
    value.is_zero() || value.bits() <= width as u64
}

#[cfg(test)]
mod tests;
