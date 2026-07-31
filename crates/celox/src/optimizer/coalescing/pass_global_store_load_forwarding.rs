//! Memory SSA and mem2reg for exact scalar SIR memory slots.
//!
//! The local forwarding pass cannot see through branches or recovered loops.
//! This pass constructs pruned SSA names for exact, non-aliased static memory
//! slots and replaces dominated loads with the reaching stored value. Observable
//! slots retain their stores. A whole-program entry point additionally promotes
//! non-escaping, definitely-defined combinational slots and removes their stores.

use super::shared::{batch_replace_in_inst, batch_replace_in_terminator};
use super::state_ssa::{StateFragment, StateSsa};
use crate::ir::cfg::SirCfg;
use crate::ir::*;
use crate::{HashMap, HashSet};

struct SlotPlan {
    analysis_slot: usize,
    fragment: StateFragment,
    ty: RegisterType,
    phi_blocks: Vec<usize>,
    promote: bool,
}

fn alloc_register(
    register_map: &mut HashMap<RegisterId, RegisterType>,
    next_register: &mut usize,
    ty: &RegisterType,
) -> RegisterId {
    while register_map.contains_key(&RegisterId(*next_register)) {
        *next_register += 1;
    }
    let register = RegisterId(*next_register);
    *next_register += 1;
    register_map.insert(register, ty.clone());
    register
}

fn append_edge_arguments(
    terminator: &mut SIRTerminator,
    successor_arguments: &HashMap<BlockId, Vec<RegisterId>>,
) {
    match terminator {
        SIRTerminator::Jump(target, arguments) => {
            if let Some(extra) = successor_arguments.get(target) {
                arguments.extend(extra);
            }
        }
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => {
            if let Some(extra) = successor_arguments.get(&true_block.0) {
                true_block.1.extend(extra);
            }
            if let Some(extra) = successor_arguments.get(&false_block.0) {
                false_block.1.extend(extra);
            }
        }
        SIRTerminator::Switch { cases, default, .. } => {
            debug_assert!(
                cases
                    .iter()
                    .map(|case| case.target)
                    .chain(std::iter::once(*default))
                    .all(|target| successor_arguments.get(&target).is_none_or(Vec::is_empty)),
                "switch edges cannot carry promoted state arguments"
            );
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

#[cfg(test)]
fn forward_stable_static_slots(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    let no_promotions = HashSet::default();
    rewrite_global_static_slots(
        eu,
        STABLE_REGION,
        PromotionPolicy::Exact(&no_promotions),
        &HashMap::default(),
        None,
    )
}

#[derive(Clone, Copy)]
enum PromotionPolicy<'a> {
    Exact(&'a HashSet<StateFragment>),
}

#[cfg(test)]
fn rewrite_global_static_slots(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    region: u32,
    promotion: PromotionPolicy<'_>,
    fallback_definitions: &HashMap<RegisterId, StateFragment>,
    eligible_load_blocks: Option<&HashSet<BlockId>>,
) -> bool {
    let mut rewritten = eu.clone();
    let mut stable_passthroughs = HashMap::default();
    let Some(changed) = rewrite_global_static_slots_in_place(
        &mut rewritten,
        region,
        promotion,
        fallback_definitions,
        eligible_load_blocks,
        &mut stable_passthroughs,
    ) else {
        return false;
    };
    if !changed || rewritten.verify_result().is_err() {
        return false;
    }
    *eu = rewritten;
    true
}

fn rewrite_global_static_slots_in_place(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    region: u32,
    promotion: PromotionPolicy<'_>,
    fallback_definitions: &HashMap<RegisterId, StateFragment>,
    eligible_load_blocks: Option<&HashSet<BlockId>>,
    stable_passthroughs: &mut HashMap<RegisterId, StateFragment>,
) -> Option<bool> {
    let cfg = SirCfg::analyze(eu).ok()?;
    let state = StateSsa::analyze(eu, &cfg, region, eligible_load_blocks).ok()?;
    let mut candidates = state
        .slots
        .iter()
        .enumerate()
        .filter_map(|(analysis_slot, slot)| {
            let selected_for_promotion = match promotion {
                PromotionPolicy::Exact(slots) => slots.contains(&slot.fragment),
            };
            let promote = selected_for_promotion
                && !slot.has_effectful_store
                && !slot.has_kill
                && !slot.escapes
                && !slot.live_in_entry;
            (!slot.phi_blocks.contains(&0)).then_some(SlotPlan {
                analysis_slot,
                fragment: slot.fragment,
                ty: slot.ty.clone(),
                phi_blocks: slot.phi_blocks.clone(),
                promote,
            })
        })
        .collect::<Vec<_>>();
    // A Switch edge deliberately carries no SSA arguments. Do not promote a
    // fragment whose iterated dominance frontier would require adding one:
    // the target remains memory-resident and other fragments are unaffected.
    candidates.retain(|candidate| {
        candidate.phi_blocks.iter().all(|&block| {
            cfg.predecessors[block].iter().all(|&predecessor| {
                !matches!(
                    eu.blocks[&cfg.block_ids[predecessor]].terminator,
                    SIRTerminator::Switch { .. }
                )
            })
        })
    });
    candidates.sort_by_key(|candidate| candidate.fragment);
    if candidates.is_empty() {
        return Some(false);
    }

    let slot_index = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| (candidate.fragment, index))
        .collect::<HashMap<_, _>>();
    let analysis_to_candidate = candidates
        .iter()
        .enumerate()
        .map(|(candidate, slot)| (slot.analysis_slot, candidate))
        .collect::<HashMap<_, _>>();
    let mut phi_slots = vec![Vec::new(); cfg.block_ids.len()];
    for (slot, candidate) in candidates.iter().enumerate() {
        for &block in &candidate.phi_blocks {
            phi_slots[block].push(slot);
        }
    }
    for slots in &mut phi_slots {
        slots.sort_unstable();
    }

    let mut next_register = eu
        .register_map
        .keys()
        .map(|register| register.0)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let mut phi_registers = vec![Vec::new(); cfg.block_ids.len()];
    for block in 0..cfg.block_ids.len() {
        for &slot in &phi_slots[block] {
            let register = alloc_register(
                &mut eu.register_map,
                &mut next_register,
                &candidates[slot].ty,
            );
            eu.blocks
                .get_mut(&cfg.block_ids[block])
                .unwrap()
                .params
                .push(register);
            phi_registers[block].push(register);
        }
    }

    #[derive(Clone, Copy)]
    enum MemoryHome {
        Slot,
        Stable,
    }

    #[derive(Clone, Copy)]
    enum ReachingValue {
        Register(RegisterId),
        Memory(MemoryHome),
    }

    enum Visit {
        Enter(usize),
        Exit(Vec<usize>),
    }
    let mut values = vec![Vec::<ReachingValue>::new(); candidates.len()];
    let mut aliases = HashMap::<RegisterId, RegisterId>::default();
    let mut visits = vec![Visit::Enter(0)];
    let mut changed = false;
    while let Some(visit) = visits.pop() {
        match visit {
            Visit::Exit(pushed_slots) => {
                for slot in pushed_slots.into_iter().rev() {
                    values[slot].pop();
                }
            }
            Visit::Enter(block_index) => {
                let block_id = cfg.block_ids[block_index];
                let mut pushed_slots = Vec::new();
                for (&slot, &register) in phi_slots[block_index]
                    .iter()
                    .zip(&phi_registers[block_index])
                {
                    values[slot].push(ReachingValue::Register(register));
                    pushed_slots.push(slot);
                }

                let old_instructions =
                    std::mem::take(&mut eu.blocks.get_mut(&block_id).unwrap().instructions);
                let mut instructions = Vec::with_capacity(old_instructions.len());
                for (instruction_index, mut instruction) in old_instructions.into_iter().enumerate()
                {
                    for analysis_slot in state.killed_slots(block_id, instruction_index) {
                        if let Some(&slot) = analysis_to_candidate.get(&analysis_slot) {
                            values[slot].push(ReachingValue::Memory(MemoryHome::Slot));
                            pushed_slots.push(slot);
                        }
                    }
                    batch_replace_in_inst(&mut instruction, &aliases);
                    match instruction {
                        SIRInstruction::Load(
                            destination,
                            addr,
                            SIROffset::Static(bit_offset),
                            width,
                        ) => {
                            let fragment = StateFragment::from_access(
                                addr,
                                bit_offset,
                                width,
                                &eu.register_map[&destination],
                            );
                            if let Some(seed_fragment) = fallback_definitions.get(&destination)
                                && let Some(&slot) = slot_index.get(seed_fragment)
                                && candidates[slot].promote
                            {
                                changed = true;
                                continue;
                            }
                            if eligible_load_blocks.is_none_or(|blocks| blocks.contains(&block_id))
                                && let Some(&slot) = slot_index.get(&fragment)
                            {
                                match values[slot].last().copied() {
                                    Some(ReachingValue::Register(value)) => {
                                        aliases.insert(destination, value);
                                        changed = true;
                                    }
                                    memory => {
                                        let mut load_addr = addr;
                                        if matches!(
                                            memory,
                                            Some(ReachingValue::Memory(MemoryHome::Stable))
                                        ) {
                                            load_addr.region = STABLE_REGION;
                                        }
                                        values[slot].push(ReachingValue::Register(destination));
                                        pushed_slots.push(slot);
                                        instructions.push(SIRInstruction::Load(
                                            destination,
                                            load_addr,
                                            SIROffset::Static(bit_offset),
                                            width,
                                        ));
                                    }
                                }
                            } else {
                                instructions.push(SIRInstruction::Load(
                                    destination,
                                    addr,
                                    SIROffset::Static(bit_offset),
                                    width,
                                ));
                            }
                        }
                        SIRInstruction::Store(
                            addr,
                            SIROffset::Static(bit_offset),
                            width,
                            source,
                            triggers,
                            capture_sites,
                        ) => {
                            let fragment = StateFragment::from_access(
                                addr,
                                bit_offset,
                                width,
                                &eu.register_map[&source],
                            );
                            if let Some(&slot) = slot_index.get(&fragment) {
                                let value = if candidates[slot].promote
                                    && fallback_definitions.get(&source) == Some(&fragment)
                                {
                                    ReachingValue::Memory(MemoryHome::Stable)
                                } else {
                                    ReachingValue::Register(source)
                                };
                                values[slot].push(value);
                                pushed_slots.push(slot);
                                if candidates[slot].promote {
                                    changed = true;
                                    continue;
                                }
                            }
                            instructions.push(SIRInstruction::Store(
                                addr,
                                SIROffset::Static(bit_offset),
                                width,
                                source,
                                triggers,
                                capture_sites,
                            ));
                        }
                        instruction => instructions.push(instruction),
                    }
                }

                let mut successor_arguments = HashMap::<BlockId, Vec<RegisterId>>::default();
                for &successor in &cfg.successors[block_index] {
                    let mut arguments = Vec::with_capacity(phi_slots[successor].len());
                    for &slot in &phi_slots[successor] {
                        let value = match values[slot].last().copied() {
                            Some(ReachingValue::Register(value)) => value,
                            current => {
                                let candidate = &candidates[slot];
                                let register = alloc_register(
                                    &mut eu.register_map,
                                    &mut next_register,
                                    &candidate.ty,
                                );
                                let mut address = candidate.fragment.addr;
                                let stable_passthrough = matches!(
                                    current,
                                    Some(ReachingValue::Memory(MemoryHome::Stable))
                                );
                                if stable_passthrough {
                                    address.region = STABLE_REGION;
                                }
                                instructions.push(SIRInstruction::Load(
                                    register,
                                    address,
                                    SIROffset::Static(candidate.fragment.bit_offset),
                                    candidate.fragment.width,
                                ));
                                if stable_passthrough {
                                    // This exact load was inserted at the
                                    // predecessor tail solely to represent an
                                    // unchanged STABLE home as a phi input.
                                    // Preserve that provenance instead of
                                    // recognizing arbitrary load/store text.
                                    let mut fragment = candidate.fragment;
                                    fragment.addr.region = STABLE_REGION;
                                    stable_passthroughs.insert(register, fragment);
                                }
                                values[slot].push(ReachingValue::Register(register));
                                pushed_slots.push(slot);
                                register
                            }
                        };
                        arguments.push(value);
                    }
                    successor_arguments.insert(cfg.block_ids[successor], arguments);
                }

                let block = eu.blocks.get_mut(&block_id).unwrap();
                block.instructions = instructions;
                batch_replace_in_terminator(&mut block.terminator, &aliases);
                append_edge_arguments(&mut block.terminator, &successor_arguments);

                visits.push(Visit::Exit(pushed_slots));
                for &child in cfg.dom_children[block_index].iter().rev() {
                    visits.push(Visit::Enter(child));
                }
            }
        }
    }
    Some(changed)
}

/// Promote exact STABLE-region slots which are entirely defined inside a
/// fused comb/FF evaluation.
///
/// A persistent RTL value is live on entry and therefore cannot be selected.
/// The remaining eligible slots are compiler-created comb temporaries: every
/// path defines them before use, they do not alias an imprecise access, and
/// their stores carry no trigger or capture effect.  Keeping those values in
/// memory would merely encode an edge in the comb dependency graph as a
/// Store/Load pair.
pub(crate) fn promote_fused_comb_static_slots(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
) -> Result<bool, String> {
    let cfg = SirCfg::analyze(eu).map_err(|error| error.to_string())?;
    let state =
        StateSsa::analyze(eu, &cfg, STABLE_REGION, None).map_err(|error| error.to_string())?;
    let promotable = state
        .slots
        .iter()
        .filter(|slot| {
            !slot.has_effectful_store
                && !slot.has_kill
                && !slot.escapes
                && !slot.live_in_entry
                && !slot.phi_blocks.contains(&0)
        })
        .map(|slot| slot.fragment)
        .collect::<HashSet<_>>();
    if promotable.is_empty() {
        return Ok(false);
    }

    let mut stable_passthroughs = HashMap::default();
    let changed = rewrite_global_static_slots_in_place(
        eu,
        STABLE_REGION,
        PromotionPolicy::Exact(&promotable),
        &HashMap::default(),
        None,
        &mut stable_passthroughs,
    )
    .ok_or_else(|| "failed to construct fused comb STABLE StateSSA".to_string())?;
    if !changed {
        return Ok(false);
    }
    eu.verify_result().map_err(|error| error.to_string())?;
    Ok(true)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct WorkingRoundTripKey {
    address: AbsoluteAddr,
    bit_offset: usize,
    width: usize,
}

impl WorkingRoundTripKey {
    fn overlaps(self, other: Self) -> bool {
        self.address == other.address
            && self.bit_offset < other.bit_offset.saturating_add(other.width)
            && other.bit_offset < self.bit_offset.saturating_add(self.width)
    }

    fn working_fragment(self, ty: &RegisterType) -> StateFragment {
        StateFragment::from_access(
            RegionedAbsoluteAddr::from_absolute_addr(WORKING_REGION, self.address),
            self.bit_offset,
            self.width,
            ty,
        )
    }
}

#[derive(Default)]
struct WorkingRoundTripFacts {
    ty: Option<RegisterType>,
    invalid: bool,
    has_seed: bool,
    has_store: bool,
    has_apply: bool,
}

impl WorkingRoundTripFacts {
    fn record_type(&mut self, ty: &RegisterType, width: usize) {
        if width == 0
            || ty.width() != width
            || self.ty.as_ref().is_some_and(|previous| previous != ty)
        {
            self.invalid = true;
        }
        self.ty.get_or_insert_with(|| ty.clone());
    }
}

fn normalize_working_commits(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    block_order: &[BlockId],
    eligible: &HashMap<WorkingRoundTripKey, (StateFragment, RegisterType)>,
    selected: &HashSet<StateFragment>,
) -> HashMap<RegisterId, StateFragment> {
    let mut next_register = eu
        .register_map
        .keys()
        .map(|register| register.0)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let mut fallback_definitions = HashMap::default();
    for &block_id in block_order {
        let block = eu.blocks.get_mut(&block_id).unwrap();
        let old_instructions = std::mem::take(&mut block.instructions);
        let mut instructions = Vec::with_capacity(old_instructions.len());
        for instruction in old_instructions {
            match instruction {
                SIRInstruction::Commit(
                    source,
                    destination,
                    SIROffset::Static(offset),
                    width,
                    triggers,
                ) if source.region == STABLE_REGION
                    && destination.region == WORKING_REGION
                    && eligible
                        .get(&WorkingRoundTripKey {
                            address: destination.absolute_addr(),
                            bit_offset: offset,
                            width,
                        })
                        .is_some_and(|(fragment, _)| selected.contains(fragment)) =>
                {
                    let key = WorkingRoundTripKey {
                        address: destination.absolute_addr(),
                        bit_offset: offset,
                        width,
                    };
                    let (fragment, ty) = &eligible[&key];
                    let register = alloc_register(&mut eu.register_map, &mut next_register, ty);
                    fallback_definitions.insert(register, *fragment);
                    instructions.push(SIRInstruction::Load(
                        register,
                        source,
                        SIROffset::Static(offset),
                        width,
                    ));
                    instructions.push(SIRInstruction::Store(
                        destination,
                        SIROffset::Static(offset),
                        width,
                        register,
                        triggers,
                        Vec::new(),
                    ));
                }
                SIRInstruction::Commit(
                    source,
                    destination,
                    SIROffset::Static(offset),
                    width,
                    triggers,
                ) if source.region == WORKING_REGION
                    && destination.region == STABLE_REGION
                    && eligible
                        .get(&WorkingRoundTripKey {
                            address: source.absolute_addr(),
                            bit_offset: offset,
                            width,
                        })
                        .is_some_and(|(fragment, _)| selected.contains(fragment)) =>
                {
                    let key = WorkingRoundTripKey {
                        address: source.absolute_addr(),
                        bit_offset: offset,
                        width,
                    };
                    let (_, ty) = &eligible[&key];
                    let register = alloc_register(&mut eu.register_map, &mut next_register, ty);
                    instructions.push(SIRInstruction::Load(
                        register,
                        source,
                        SIROffset::Static(offset),
                        width,
                    ));
                    instructions.push(SIRInstruction::Store(
                        destination,
                        SIROffset::Static(offset),
                        width,
                        register,
                        triggers,
                        Vec::new(),
                    ));
                }
                instruction => instructions.push(instruction),
            }
        }
        block.instructions = instructions;
    }
    fallback_definitions
}

/// Promote the ordinary WORKING-region round trip in a merged eval_apply_ff:
///
/// `Commit(STABLE→WORKING)` becomes the SSA live-in, WORKING stores become SSA
/// definitions, and `Commit(WORKING→STABLE)` becomes the sole writeback. This
/// handles any number of disjoint exact fragments. Sparse, dynamic, overlapping,
/// escaping, or effectful next-state storage keeps its memory representation.
pub(crate) fn promote_eval_apply_working_round_trips(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
) -> bool {
    let Ok(cfg) = SirCfg::analyze(eu) else {
        return false;
    };
    let mut facts = HashMap::<WorkingRoundTripKey, WorkingRoundTripFacts>::default();
    let mut invalid_addresses = HashSet::<AbsoluteAddr>::default();
    for &block_id in &cfg.block_ids {
        let block = &eu.blocks[&block_id];
        for instruction in &block.instructions {
            match instruction {
                SIRInstruction::Load(destination, address, SIROffset::Static(offset), width)
                    if address.region == WORKING_REGION =>
                {
                    let key = WorkingRoundTripKey {
                        address: address.absolute_addr(),
                        bit_offset: *offset,
                        width: *width,
                    };
                    facts
                        .entry(key)
                        .or_default()
                        .record_type(&eu.register_map[destination], *width);
                }
                SIRInstruction::Store(
                    address,
                    SIROffset::Static(offset),
                    width,
                    source,
                    triggers,
                    capture_sites,
                ) if address.region == WORKING_REGION => {
                    let key = WorkingRoundTripKey {
                        address: address.absolute_addr(),
                        bit_offset: *offset,
                        width: *width,
                    };
                    let entry = facts.entry(key).or_default();
                    entry.record_type(&eu.register_map[source], *width);
                    entry.has_store = true;
                    entry.invalid |= !triggers.is_empty() || !capture_sites.is_empty();
                }
                SIRInstruction::Commit(
                    source,
                    destination,
                    SIROffset::Static(offset),
                    width,
                    triggers,
                ) if source.region == STABLE_REGION
                    && destination.region == WORKING_REGION
                    && source.absolute_addr() == destination.absolute_addr() =>
                {
                    let key = WorkingRoundTripKey {
                        address: destination.absolute_addr(),
                        bit_offset: *offset,
                        width: *width,
                    };
                    let entry = facts.entry(key).or_default();
                    entry.has_seed = true;
                    entry.invalid |= *width == 0 || !triggers.is_empty();
                }
                SIRInstruction::Commit(
                    source,
                    destination,
                    SIROffset::Static(offset),
                    width,
                    _,
                ) if source.region == WORKING_REGION
                    && destination.region == STABLE_REGION
                    && source.absolute_addr() == destination.absolute_addr() =>
                {
                    let key = WorkingRoundTripKey {
                        address: source.absolute_addr(),
                        bit_offset: *offset,
                        width: *width,
                    };
                    let entry = facts.entry(key).or_default();
                    entry.has_apply = true;
                    entry.invalid |= *width == 0;
                }
                SIRInstruction::Load(_, address, _, _)
                | SIRInstruction::Store(address, _, _, _, _, _)
                    if address.region == WORKING_REGION =>
                {
                    invalid_addresses.insert(address.absolute_addr());
                }
                SIRInstruction::Commit(source, destination, _, _, _)
                    if source.region == WORKING_REGION || destination.region == WORKING_REGION =>
                {
                    if source.region == WORKING_REGION {
                        invalid_addresses.insert(source.absolute_addr());
                    }
                    if destination.region == WORKING_REGION {
                        invalid_addresses.insert(destination.absolute_addr());
                    }
                }
                _ => {}
            }
        }
    }

    let mut keys = facts.keys().copied().collect::<Vec<_>>();
    keys.sort_unstable();
    for left in 0..keys.len() {
        let left_end = keys[left].bit_offset.saturating_add(keys[left].width);
        let mut right = left + 1;
        while right < keys.len()
            && keys[left].address == keys[right].address
            && keys[right].bit_offset < left_end
        {
            debug_assert!(keys[left].overlaps(keys[right]));
            facts.get_mut(&keys[left]).unwrap().invalid = true;
            facts.get_mut(&keys[right]).unwrap().invalid = true;
            right += 1;
        }
    }
    for (key, entry) in &mut facts {
        entry.invalid |= invalid_addresses.contains(&key.address);
    }

    let eligible = keys
        .into_iter()
        .filter_map(|key| {
            let entry = &facts[&key];
            if entry.invalid || !entry.has_seed || !entry.has_store || !entry.has_apply {
                return None;
            }
            let ty = entry.ty.clone()?;
            Some((key, (key.working_fragment(&ty), ty)))
        })
        .collect::<HashMap<_, _>>();
    if eligible.is_empty() {
        return false;
    }

    let all_slots = eligible
        .values()
        .map(|(fragment, _)| *fragment)
        .collect::<HashSet<_>>();
    let mut preview = eu.clone();
    normalize_working_commits(&mut preview, &cfg.block_ids, &eligible, &all_slots);
    let Ok(preview_cfg) = SirCfg::analyze(&preview) else {
        return false;
    };
    let Ok(preview_state) = StateSsa::analyze(&preview, &preview_cfg, WORKING_REGION, None) else {
        return false;
    };
    let slots = preview_state
        .slots
        .iter()
        .filter(|slot| {
            all_slots.contains(&slot.fragment)
                && !slot.has_effectful_store
                && !slot.has_kill
                && !slot.escapes
                && !slot.live_in_entry
                && !slot.phi_blocks.contains(&0)
        })
        .map(|slot| slot.fragment)
        .collect::<HashSet<_>>();
    if slots.is_empty() {
        return false;
    }

    // Normalize and rewrite only proven promotable fragments on a fresh clone.
    // This keeps rejected fragments byte-for-byte in their original form.
    let mut rewritten = eu.clone();
    let fallback_definitions =
        normalize_working_commits(&mut rewritten, &cfg.block_ids, &eligible, &slots);
    let mut stable_passthroughs = HashMap::default();
    let Some(changed) = rewrite_global_static_slots_in_place(
        &mut rewritten,
        WORKING_REGION,
        PromotionPolicy::Exact(&slots),
        &fallback_definitions,
        None,
        &mut stable_passthroughs,
    ) else {
        return false;
    };
    if !changed {
        return false;
    }
    sink_phi_writebacks_to_predecessors(&mut rewritten, &stable_passthroughs);
    if rewritten.verify_result().is_err() {
        return false;
    }
    *eu = rewritten;
    true
}

fn add_register_use(counts: &mut HashMap<RegisterId, usize>, register: RegisterId) {
    *counts.entry(register).or_default() += 1;
}

fn count_register_uses(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<RegisterId, usize> {
    let mut counts = HashMap::default();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            match instruction {
                SIRInstruction::Imm(..) => {}
                SIRInstruction::Load(_, _, offset, _) => {
                    for register in offset.dynamic_registers().into_iter().flatten() {
                        add_register_use(&mut counts, register);
                    }
                }
                SIRInstruction::Binary(_, lhs, _, rhs) => {
                    add_register_use(&mut counts, *lhs);
                    add_register_use(&mut counts, *rhs);
                }
                SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, _, _) => {
                    add_register_use(&mut counts, *source);
                }
                SIRInstruction::Store(_, offset, _, source, _, _) => {
                    add_register_use(&mut counts, *source);
                    for register in offset.dynamic_registers().into_iter().flatten() {
                        add_register_use(&mut counts, register);
                    }
                }
                SIRInstruction::Commit(_, _, offset, _, _) => {
                    for register in offset.dynamic_registers().into_iter().flatten() {
                        add_register_use(&mut counts, register);
                    }
                }
                SIRInstruction::Concat(_, sources) => {
                    for &source in sources {
                        add_register_use(&mut counts, source);
                    }
                }
                SIRInstruction::LaneAggregate { inputs, .. } => {
                    for &input in inputs {
                        add_register_use(&mut counts, input);
                    }
                }
                SIRInstruction::Mux(_, condition, then_value, else_value) => {
                    add_register_use(&mut counts, *condition);
                    add_register_use(&mut counts, *then_value);
                    add_register_use(&mut counts, *else_value);
                }
                SIRInstruction::RuntimeEvent { args, .. }
                | SIRInstruction::CombCaptureEvent { args, .. } => {
                    for &argument in args {
                        add_register_use(&mut counts, argument);
                    }
                }
                SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
                    add_register_use(&mut counts, *old);
                    add_register_use(&mut counts, *new);
                }
            }
        }
        match &block.terminator {
            SIRTerminator::Jump(_, arguments) => {
                for &argument in arguments {
                    add_register_use(&mut counts, argument);
                }
            }
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                add_register_use(&mut counts, *cond);
                for &argument in true_block.1.iter().chain(&false_block.1) {
                    add_register_use(&mut counts, argument);
                }
            }
            SIRTerminator::Switch { selector, .. } => {
                add_register_use(&mut counts, *selector);
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    counts
}

fn ranges_overlap(
    left_offset: usize,
    left_width: usize,
    right_offset: usize,
    right_width: usize,
) -> bool {
    left_offset < right_offset.saturating_add(right_width)
        && right_offset < left_offset.saturating_add(left_width)
}

fn instruction_blocks_writeback_motion(
    instruction: &SIRInstruction<RegionedAbsoluteAddr>,
    address: RegionedAbsoluteAddr,
    offset: usize,
    width: usize,
) -> bool {
    let aliases = |other: &RegionedAbsoluteAddr, other_offset: &SIROffset, other_width: usize| {
        if other.absolute_addr() != address.absolute_addr() {
            return false;
        }
        match other_offset {
            SIROffset::Static(other_offset)
            | SIROffset::PackedElements {
                bit_offset: other_offset,
                ..
            } => ranges_overlap(offset, width, *other_offset, other_width),
            SIROffset::Dynamic(_) | SIROffset::Element { .. } => true,
        }
    };
    match instruction {
        SIRInstruction::Load(_, other, other_offset, other_width)
        | SIRInstruction::Store(other, other_offset, other_width, _, _, _) => {
            aliases(other, other_offset, *other_width)
        }
        SIRInstruction::Commit(source, destination, other_offset, other_width, _) => {
            aliases(source, other_offset, *other_width)
                || aliases(destination, other_offset, *other_width)
        }
        SIRInstruction::RuntimeEvent { .. }
        | SIRInstruction::CombCaptureEvent { .. }
        | SIRInstruction::CombCaptureEnableIfChanged { .. } => true,
        _ => false,
    }
}

/// A writeback whose only operand is a merge value does not need an actual
/// phi copy. Put the writeback on each single-successor incoming edge instead.
/// Repeating this peels chains of merge-only live ranges back to their defs.
fn sink_phi_writebacks_to_predecessors(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    stable_passthroughs: &HashMap<RegisterId, StateFragment>,
) -> bool {
    struct Candidate {
        instruction: usize,
        parameter: usize,
        register: RegisterId,
        edge_stores: Vec<(BlockId, Option<SIRInstruction<RegionedAbsoluteAddr>>)>,
    }

    let mut changed = false;
    while let Ok(cfg) = SirCfg::analyze(eu) {
        let use_counts = count_register_uses(eu);
        let mut rewrite = None;

        'blocks: for block_index in 0..cfg.block_ids.len() {
            let block_id = cfg.block_ids[block_index];
            let block = &eu.blocks[&block_id];
            if block.params.is_empty() || cfg.predecessors[block_index].is_empty() {
                continue;
            }
            if cfg.predecessors[block_index].iter().any(|&predecessor| {
                cfg.successors[predecessor].len() != 1
                    || !matches!(
                        eu.blocks[&cfg.block_ids[predecessor]].terminator,
                        SIRTerminator::Jump(target, _) if target == block_id
                    )
            }) {
                continue;
            }

            let mut candidates = Vec::new();
            for (instruction_index, instruction) in block.instructions.iter().enumerate() {
                let SIRInstruction::Store(
                    address,
                    SIROffset::Static(offset),
                    width,
                    source,
                    triggers,
                    capture_sites,
                ) = instruction
                else {
                    continue;
                };
                let Some(parameter_index) = block.params.iter().position(|param| param == source)
                else {
                    continue;
                };
                if use_counts.get(source).copied() != Some(1)
                    || block.instructions[..instruction_index].iter().any(|prior| {
                        instruction_blocks_writeback_motion(prior, *address, *offset, *width)
                    })
                {
                    continue;
                }

                let mut edge_stores = Vec::with_capacity(cfg.predecessors[block_index].len());
                for &predecessor in &cfg.predecessors[block_index] {
                    let predecessor_id = cfg.block_ids[predecessor];
                    let SIRTerminator::Jump(_, arguments) = &eu.blocks[&predecessor_id].terminator
                    else {
                        continue 'blocks;
                    };
                    let Some(&incoming) = arguments.get(parameter_index) else {
                        continue 'blocks;
                    };
                    let writeback_fragment = StateFragment::from_access(
                        *address,
                        *offset,
                        *width,
                        &eu.register_map[&incoming],
                    );
                    // An ordinary load of the same address is not enough:
                    // only a tail load created above proves that this edge
                    // carries the untouched STABLE value. Trigger/capture
                    // effects still require the writeback even for that value.
                    let unchanged_stable_value = triggers.is_empty()
                        && capture_sites.is_empty()
                        && stable_passthroughs.get(&incoming) == Some(&writeback_fragment);
                    edge_stores.push((
                        predecessor_id,
                        (!unchanged_stable_value).then(|| {
                            SIRInstruction::Store(
                                *address,
                                SIROffset::Static(*offset),
                                *width,
                                incoming,
                                triggers.clone(),
                                capture_sites.clone(),
                            )
                        }),
                    ));
                }
                candidates.push(Candidate {
                    instruction: instruction_index,
                    parameter: parameter_index,
                    register: *source,
                    edge_stores,
                });
            }
            if !candidates.is_empty() {
                rewrite = Some((block_id, candidates));
                break 'blocks;
            }
        }

        let Some((block_id, mut candidates)) = rewrite else {
            break;
        };
        for candidate in &candidates {
            for (predecessor, store) in &candidate.edge_stores {
                if let Some(store) = store {
                    eu.blocks
                        .get_mut(predecessor)
                        .unwrap()
                        .instructions
                        .push(store.clone());
                }
            }
        }
        candidates.sort_unstable_by_key(|candidate| candidate.parameter);
        for candidate in candidates.iter().rev() {
            for (predecessor, _) in &candidate.edge_stores {
                let SIRTerminator::Jump(_, arguments) =
                    &mut eu.blocks.get_mut(predecessor).unwrap().terminator
                else {
                    unreachable!("writeback motion accepted only Jump predecessors");
                };
                arguments.remove(candidate.parameter);
            }
            eu.blocks
                .get_mut(&block_id)
                .unwrap()
                .params
                .remove(candidate.parameter);
            eu.register_map.remove(&candidate.register);
        }
        candidates.sort_unstable_by_key(|candidate| candidate.instruction);
        for candidate in candidates.iter().rev() {
            eu.blocks
                .get_mut(&block_id)
                .unwrap()
                .instructions
                .remove(candidate.instruction);
        }
        changed = true;
    }

    let uses = count_register_uses(eu);
    // Omitted writebacks make their synthetic phi-input loads dead. Remove
    // only definitions carrying the pass-local provenance recorded above.
    let dead_passthroughs = stable_passthroughs
        .keys()
        .copied()
        .filter(|register| !uses.contains_key(register))
        .collect::<HashSet<_>>();
    if !dead_passthroughs.is_empty() {
        for block in eu.blocks.values_mut() {
            block.instructions.retain(|instruction| {
                !matches!(
                    instruction,
                    SIRInstruction::Load(destination, _, _, _)
                        if dead_passthroughs.contains(destination)
                )
            });
        }
        for register in dead_passthroughs {
            eu.register_map.remove(&register);
        }
        changed = true;
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{BasicBlock, InstanceId};
    use veryl_analyzer::ir::VarId;

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
    }

    fn address(_var: u32) -> RegionedAbsoluteAddr {
        address_in_instance(0)
    }

    fn address_in_instance(instance: usize) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: InstanceId(instance),
            var_id: VarId::default(),
        }
    }

    fn unit(
        blocks: impl IntoIterator<Item = BasicBlock<RegionedAbsoluteAddr>>,
        registers: impl IntoIterator<Item = (RegisterId, RegisterType)>,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: blocks.into_iter().map(|block| (block.id, block)).collect(),
            register_map: registers.into_iter().collect(),
        }
    }

    #[test]
    fn forwards_across_a_basic_block_boundary() {
        let addr = address(0);
        let mut eu = unit(
            [
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0)],
                    instructions: vec![SIRInstruction::Store(
                        addr,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
                },
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 8),
                        SIRInstruction::Unary(RegisterId(2), UnaryOp::Ident, RegisterId(1)),
                    ],
                    terminator: SIRTerminator::Return,
                },
            ],
            [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
            ],
        );

        assert!(forward_stable_static_slots(&mut eu));
        eu.verify_result().unwrap();
        assert!(
            eu.blocks[&BlockId(1)]
                .instructions
                .iter()
                .all(|instruction| !matches!(instruction, SIRInstruction::Load(..)))
        );
        assert!(matches!(
            eu.blocks[&BlockId(1)].instructions[0],
            SIRInstruction::Unary(_, UnaryOp::Ident, RegisterId(0))
        ));
    }

    #[test]
    fn inserts_a_pruned_phi_at_a_diamond_join() {
        let addr = address(0);
        let mut eu = unit(
            [
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Store(
                        addr,
                        SIROffset::Static(0),
                        8,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
                },
                BasicBlock {
                    id: BlockId(2),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Store(
                        addr,
                        SIROffset::Static(0),
                        8,
                        RegisterId(2),
                        Vec::new(),
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
                },
                BasicBlock {
                    id: BlockId(3),
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Load(RegisterId(3), addr, SIROffset::Static(0), 8),
                        SIRInstruction::Unary(RegisterId(4), UnaryOp::Ident, RegisterId(3)),
                    ],
                    terminator: SIRTerminator::Return,
                },
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(8)),
                (RegisterId(4), bit(8)),
            ],
        );

        assert!(forward_stable_static_slots(&mut eu));
        eu.verify_result().unwrap();
        let join_param = eu.blocks[&BlockId(3)].params[0];
        assert!(matches!(
            &eu.blocks[&BlockId(1)].terminator,
            SIRTerminator::Jump(BlockId(3), arguments) if arguments == &[RegisterId(1)]
        ));
        assert!(matches!(
            &eu.blocks[&BlockId(2)].terminator,
            SIRTerminator::Jump(BlockId(3), arguments) if arguments == &[RegisterId(2)]
        ));
        assert!(matches!(
            eu.blocks[&BlockId(3)].instructions[0],
            SIRInstruction::Unary(_, UnaryOp::Ident, source) if source == join_param
        ));
    }

    #[test]
    fn loads_the_original_value_on_a_clean_diamond_edge() {
        let addr = address(0);
        let mut eu = unit(
            [
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1)],
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Store(
                        addr,
                        SIROffset::Static(0),
                        8,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
                },
                BasicBlock {
                    id: BlockId(2),
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
                },
                BasicBlock {
                    id: BlockId(3),
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Load(RegisterId(2), addr, SIROffset::Static(0), 8),
                        SIRInstruction::Unary(RegisterId(3), UnaryOp::Ident, RegisterId(2)),
                    ],
                    terminator: SIRTerminator::Return,
                },
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(8)),
            ],
        );

        assert!(forward_stable_static_slots(&mut eu));
        eu.verify_result().unwrap();
        assert!(matches!(
            eu.blocks[&BlockId(2)].instructions.as_slice(),
            [SIRInstruction::Load(_, loaded_addr, SIROffset::Static(0), 8)] if *loaded_addr == addr
        ));
    }

    #[test]
    fn rejects_an_address_with_multiple_static_shapes() {
        let addr = address(0);
        let mut eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0)],
                instructions: vec![
                    SIRInstruction::Store(
                        addr,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 4),
                ],
                terminator: SIRTerminator::Return,
            }],
            [(RegisterId(0), bit(8)), (RegisterId(1), bit(4))],
        );

        assert!(!forward_stable_static_slots(&mut eu));
        eu.verify_result().unwrap();
    }

    #[test]
    fn forwards_independent_fragments_of_the_same_address() {
        let addr = address(0);
        let mut eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1)],
                instructions: vec![
                    SIRInstruction::Store(
                        addr,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Store(
                        addr,
                        SIROffset::Static(8),
                        8,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Load(RegisterId(2), addr, SIROffset::Static(0), 8),
                    SIRInstruction::Load(RegisterId(3), addr, SIROffset::Static(8), 8),
                    SIRInstruction::Unary(RegisterId(4), UnaryOp::Ident, RegisterId(2)),
                    SIRInstruction::Unary(RegisterId(5), UnaryOp::Ident, RegisterId(3)),
                ],
                terminator: SIRTerminator::Return,
            }],
            (0..6).map(|register| (RegisterId(register), bit(8))),
        );

        assert!(forward_stable_static_slots(&mut eu));
        eu.verify_result().unwrap();
        assert!(
            eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .all(|instruction| !matches!(instruction, SIRInstruction::Load(..)))
        );
        assert!(matches!(
            &eu.blocks[&BlockId(0)].instructions[2..],
            [
                SIRInstruction::Unary(_, UnaryOp::Ident, RegisterId(0)),
                SIRInstruction::Unary(_, UnaryOp::Ident, RegisterId(1)),
            ]
        ));
    }

    #[test]
    fn overlapping_fragment_store_kills_the_wider_slot() {
        let addr = address(0);
        let mut eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1)],
                instructions: vec![
                    SIRInstruction::Store(
                        addr,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Store(
                        addr,
                        SIROffset::Static(4),
                        4,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Load(RegisterId(2), addr, SIROffset::Static(0), 8),
                    SIRInstruction::Unary(RegisterId(3), UnaryOp::Ident, RegisterId(2)),
                ],
                terminator: SIRTerminator::Return,
            }],
            [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(4)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(8)),
            ],
        );

        assert!(!forward_stable_static_slots(&mut eu));
        eu.verify_result().unwrap();
        assert!(matches!(
            eu.blocks[&BlockId(0)].instructions[2],
            SIRInstruction::Load(..)
        ));
    }

    #[test]
    fn commit_source_read_does_not_kill_a_stable_fragment() {
        let stable = address(0);
        let mut working = stable;
        working.region = WORKING_REGION;
        let mut eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0)],
                instructions: vec![
                    SIRInstruction::Store(
                        stable,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Commit(stable, working, SIROffset::Static(0), 8, Vec::new()),
                    SIRInstruction::Load(RegisterId(1), stable, SIROffset::Static(0), 8),
                    SIRInstruction::Unary(RegisterId(2), UnaryOp::Ident, RegisterId(1)),
                ],
                terminator: SIRTerminator::Return,
            }],
            [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
            ],
        );

        assert!(forward_stable_static_slots(&mut eu));
        eu.verify_result().unwrap();
        assert!(matches!(
            eu.blocks[&BlockId(0)].instructions[2],
            SIRInstruction::Unary(_, UnaryOp::Ident, RegisterId(0))
        ));
    }

    #[test]
    fn promotes_eval_apply_working_round_trip_without_an_unused_seed_load() {
        let stable = address(0);
        let mut working = stable;
        working.region = WORKING_REGION;
        let mut eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0)],
                instructions: vec![
                    SIRInstruction::Commit(stable, working, SIROffset::Static(0), 8, Vec::new()),
                    SIRInstruction::Store(
                        working,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Commit(working, stable, SIROffset::Static(0), 8, Vec::new()),
                ],
                terminator: SIRTerminator::Return,
            }],
            [(RegisterId(0), bit(8))],
        );

        assert!(promote_eval_apply_working_round_trips(&mut eu));
        eu.verify_result().unwrap();
        assert!(
            eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .all(|instruction| {
                    !matches!(
                        instruction,
                        SIRInstruction::Load(_, address, _, _)
                            | SIRInstruction::Store(address, _, _, _, _, _)
                            if address.region == WORKING_REGION
                    ) && !matches!(instruction, SIRInstruction::Commit(..))
                })
        );
        assert!(
            eu.blocks[&BlockId(0)]
                .instructions
                .iter()
                .all(|instruction| !matches!(instruction, SIRInstruction::Load(..)))
        );
        assert!(matches!(
            eu.blocks[&BlockId(0)].instructions.last(),
            Some(SIRInstruction::Store(address, SIROffset::Static(0), 8, RegisterId(0), _, _))
                if *address == stable
        ));
    }

    #[test]
    fn eval_apply_writeback_eliminates_a_merge_only_phi() {
        let stable = address(0);
        let mut working = stable;
        working.region = WORKING_REGION;
        let mut eu = unit(
            [
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1)],
                    instructions: vec![SIRInstruction::Commit(
                        stable,
                        working,
                        SIROffset::Static(0),
                        8,
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Store(
                        working,
                        SIROffset::Static(0),
                        8,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
                },
                BasicBlock {
                    id: BlockId(2),
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
                },
                BasicBlock {
                    id: BlockId(3),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Commit(
                        working,
                        stable,
                        SIROffset::Static(0),
                        8,
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Return,
                },
            ],
            [(RegisterId(0), bit(1)), (RegisterId(1), bit(8))],
        );

        assert!(promote_eval_apply_working_round_trips(&mut eu));
        eu.verify_result().unwrap();
        assert!(eu.blocks[&BlockId(3)].params.is_empty());
        assert!(eu.blocks[&BlockId(3)].instructions.is_empty());
        assert!(matches!(
            eu.blocks[&BlockId(1)].instructions.as_slice(),
            [SIRInstruction::Store(address, SIROffset::Static(0), 8, RegisterId(1), _, _)]
                if *address == stable
        ));
        assert!(eu.blocks[&BlockId(2)].instructions.is_empty());
    }

    #[test]
    fn writeback_keeps_an_unchanged_stable_value_when_it_has_triggers() {
        let stable = address(0);
        let trigger = TriggerIdWithKind {
            kind: DomainKind::Other,
            id: 7,
        };
        let mut eu = unit(
            [
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1)],
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(BlockId(3), vec![RegisterId(1)]),
                },
                BasicBlock {
                    id: BlockId(2),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Load(
                        RegisterId(2),
                        stable,
                        SIROffset::Static(0),
                        8,
                    )],
                    terminator: SIRTerminator::Jump(BlockId(3), vec![RegisterId(2)]),
                },
                BasicBlock {
                    id: BlockId(3),
                    params: vec![RegisterId(3)],
                    instructions: vec![SIRInstruction::Store(
                        stable,
                        SIROffset::Static(0),
                        8,
                        RegisterId(3),
                        vec![trigger],
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Return,
                },
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(8)),
            ],
        );
        let mut passthroughs = HashMap::default();
        passthroughs.insert(
            RegisterId(2),
            StateFragment::from_access(stable, 0, 8, &bit(8)),
        );

        assert!(sink_phi_writebacks_to_predecessors(&mut eu, &passthroughs));
        eu.verify_result().unwrap();
        assert!(matches!(
            eu.blocks[&BlockId(2)].instructions.as_slice(),
            [
                SIRInstruction::Load(RegisterId(2), _, SIROffset::Static(0), 8),
                SIRInstruction::Store(_, SIROffset::Static(0), 8, RegisterId(2), triggers, _),
            ] if triggers == &[trigger]
        ));
    }

    #[test]
    fn writeback_motion_preserves_a_phi_used_as_a_dynamic_load_offset() {
        let writeback = address_in_instance(0);
        let indexed = address_in_instance(1);
        let mut eu = unit(
            [
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(BlockId(3), vec![RegisterId(1)]),
                },
                BasicBlock {
                    id: BlockId(2),
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(BlockId(3), vec![RegisterId(2)]),
                },
                BasicBlock {
                    id: BlockId(3),
                    params: vec![RegisterId(3)],
                    instructions: vec![
                        SIRInstruction::Load(
                            RegisterId(4),
                            indexed,
                            SIROffset::Dynamic(RegisterId(3)),
                            1,
                        ),
                        SIRInstruction::Store(
                            writeback,
                            SIROffset::Static(0),
                            8,
                            RegisterId(3),
                            Vec::new(),
                            Vec::new(),
                        ),
                    ],
                    terminator: SIRTerminator::Return,
                },
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(8)),
                (RegisterId(4), bit(1)),
            ],
        );

        eu.verify_result().unwrap();
        assert!(!sink_phi_writebacks_to_predecessors(
            &mut eu,
            &HashMap::default()
        ));
        eu.verify_result().unwrap();
    }

    #[test]
    fn forwards_after_an_overlapping_kill_is_exactly_redefined() {
        let addr = address(0);
        let mut eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                instructions: vec![
                    SIRInstruction::Store(
                        addr,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Store(
                        addr,
                        SIROffset::Static(4),
                        4,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Store(
                        addr,
                        SIROffset::Static(0),
                        8,
                        RegisterId(2),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Load(RegisterId(3), addr, SIROffset::Static(0), 8),
                    SIRInstruction::Unary(RegisterId(4), UnaryOp::Ident, RegisterId(3)),
                ],
                terminator: SIRTerminator::Return,
            }],
            [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(4)),
                (RegisterId(2), bit(8)),
                (RegisterId(3), bit(8)),
                (RegisterId(4), bit(8)),
            ],
        );

        assert!(forward_stable_static_slots(&mut eu));
        eu.verify_result().unwrap();
        assert!(matches!(
            eu.blocks[&BlockId(0)].instructions.last(),
            Some(SIRInstruction::Unary(_, UnaryOp::Ident, RegisterId(2)))
        ));
    }

    #[test]
    fn path_local_kill_materializes_only_the_killed_phi_edge() {
        let addr = address(0);
        let mut eu = unit(
            [
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1), RegisterId(2)],
                    instructions: vec![SIRInstruction::Store(
                        addr,
                        SIROffset::Static(0),
                        8,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(1), Vec::new()),
                        false_block: (BlockId(2), Vec::new()),
                    },
                },
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Store(
                        addr,
                        SIROffset::Static(4),
                        4,
                        RegisterId(2),
                        Vec::new(),
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
                },
                BasicBlock {
                    id: BlockId(2),
                    params: Vec::new(),
                    instructions: Vec::new(),
                    terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
                },
                BasicBlock {
                    id: BlockId(3),
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Load(RegisterId(3), addr, SIROffset::Static(0), 8),
                        SIRInstruction::Unary(RegisterId(4), UnaryOp::Ident, RegisterId(3)),
                    ],
                    terminator: SIRTerminator::Return,
                },
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(4)),
                (RegisterId(3), bit(8)),
                (RegisterId(4), bit(8)),
            ],
        );

        assert!(forward_stable_static_slots(&mut eu));
        eu.verify_result().unwrap();
        assert!(matches!(
            eu.blocks[&BlockId(1)].instructions.as_slice(),
            [SIRInstruction::Store(..), SIRInstruction::Load(_, loaded, SIROffset::Static(0), 8)]
                if *loaded == addr
        ));
        assert!(eu.blocks[&BlockId(2)].instructions.is_empty());
        assert_eq!(eu.blocks[&BlockId(3)].params.len(), 1);
        assert!(matches!(
            eu.blocks[&BlockId(3)].instructions.as_slice(),
            [SIRInstruction::Unary(_, UnaryOp::Ident, source)]
                if *source == eu.blocks[&BlockId(3)].params[0]
        ));
    }

    #[test]
    fn promotes_multiple_disjoint_working_fragments() {
        let stable = address(0);
        let mut working = stable;
        working.region = WORKING_REGION;
        let mut eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0), RegisterId(1)],
                instructions: vec![
                    SIRInstruction::Commit(stable, working, SIROffset::Static(0), 8, Vec::new()),
                    SIRInstruction::Commit(stable, working, SIROffset::Static(8), 8, Vec::new()),
                    SIRInstruction::Store(
                        working,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Store(
                        working,
                        SIROffset::Static(8),
                        8,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Commit(working, stable, SIROffset::Static(0), 8, Vec::new()),
                    SIRInstruction::Commit(working, stable, SIROffset::Static(8), 8, Vec::new()),
                ],
                terminator: SIRTerminator::Return,
            }],
            [(RegisterId(0), bit(8)), (RegisterId(1), bit(8))],
        );

        assert!(promote_eval_apply_working_round_trips(&mut eu));
        eu.verify_result().unwrap();
        assert_eq!(eu.blocks[&BlockId(0)].instructions.len(), 2);
        assert!(eu.blocks[&BlockId(0)].instructions.iter().all(|instruction| {
            matches!(instruction, SIRInstruction::Store(addr, _, 8, _, _, _) if addr.region == STABLE_REGION)
        }));
    }

    #[test]
    fn promotion_preserves_an_old_stable_read_until_apply() {
        let stable = address(0);
        let mut working = stable;
        working.region = WORKING_REGION;
        let mut eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0)],
                instructions: vec![
                    SIRInstruction::Commit(stable, working, SIROffset::Static(0), 8, Vec::new()),
                    SIRInstruction::Store(
                        working,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        Vec::new(),
                    ),
                    SIRInstruction::Load(RegisterId(1), stable, SIROffset::Static(0), 8),
                    SIRInstruction::Unary(RegisterId(2), UnaryOp::Ident, RegisterId(1)),
                    SIRInstruction::Commit(working, stable, SIROffset::Static(0), 8, Vec::new()),
                ],
                terminator: SIRTerminator::Return,
            }],
            [
                (RegisterId(0), bit(8)),
                (RegisterId(1), bit(8)),
                (RegisterId(2), bit(8)),
            ],
        );

        assert!(promote_eval_apply_working_round_trips(&mut eu));
        eu.verify_result().unwrap();
        let instructions = &eu.blocks[&BlockId(0)].instructions;
        let old_read = instructions
            .iter()
            .position(|instruction| {
                matches!(instruction, SIRInstruction::Load(RegisterId(1), addr, _, 8) if *addr == stable)
            })
            .unwrap();
        let apply = instructions
            .iter()
            .position(|instruction| {
                matches!(instruction, SIRInstruction::Store(addr, _, 8, RegisterId(0), _, _) if *addr == stable)
            })
            .unwrap();
        assert!(old_read < apply);
    }

    #[test]
    fn effectful_working_store_rejects_promotion_without_mutation() {
        let stable = address(0);
        let mut working = stable;
        working.region = WORKING_REGION;
        let mut eu = unit(
            [BasicBlock {
                id: BlockId(0),
                params: vec![RegisterId(0)],
                instructions: vec![
                    SIRInstruction::Commit(stable, working, SIROffset::Static(0), 8, Vec::new()),
                    SIRInstruction::Store(
                        working,
                        SIROffset::Static(0),
                        8,
                        RegisterId(0),
                        Vec::new(),
                        vec![7],
                    ),
                    SIRInstruction::Commit(working, stable, SIROffset::Static(0), 8, Vec::new()),
                ],
                terminator: SIRTerminator::Return,
            }],
            [(RegisterId(0), RegisterType::Logic { width: 8 })],
        );
        let before = eu.blocks.clone();
        let registers_before = eu.register_map.clone();

        assert!(!promote_eval_apply_working_round_trips(&mut eu));
        assert_eq!(eu.blocks, before);
        assert_eq!(eu.register_map, registers_before);
    }

    #[test]
    fn promotes_loop_carried_four_state_working_value() {
        let stable = address(0);
        let mut working = stable;
        working.region = WORKING_REGION;
        let logic = RegisterType::Logic { width: 8 };
        let mut eu = unit(
            [
                BasicBlock {
                    id: BlockId(0),
                    params: vec![RegisterId(0), RegisterId(1)],
                    instructions: vec![SIRInstruction::Commit(
                        stable,
                        working,
                        SIROffset::Static(0),
                        8,
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
                },
                BasicBlock {
                    id: BlockId(1),
                    params: Vec::new(),
                    instructions: vec![
                        SIRInstruction::Load(RegisterId(2), working, SIROffset::Static(0), 8),
                        SIRInstruction::Unary(RegisterId(3), UnaryOp::Ident, RegisterId(2)),
                    ],
                    terminator: SIRTerminator::Branch {
                        cond: RegisterId(0),
                        true_block: (BlockId(2), Vec::new()),
                        false_block: (BlockId(3), Vec::new()),
                    },
                },
                BasicBlock {
                    id: BlockId(2),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Store(
                        working,
                        SIROffset::Static(0),
                        8,
                        RegisterId(1),
                        Vec::new(),
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
                },
                BasicBlock {
                    id: BlockId(3),
                    params: Vec::new(),
                    instructions: vec![SIRInstruction::Commit(
                        working,
                        stable,
                        SIROffset::Static(0),
                        8,
                        Vec::new(),
                    )],
                    terminator: SIRTerminator::Return,
                },
            ],
            [
                (RegisterId(0), bit(1)),
                (RegisterId(1), logic.clone()),
                (RegisterId(2), logic.clone()),
                (RegisterId(3), logic),
            ],
        );

        assert!(promote_eval_apply_working_round_trips(&mut eu));
        eu.verify_result().unwrap();
        assert!(eu.blocks[&BlockId(1)].params.len() == 1);
        assert!(
            eu.blocks
                .values()
                .all(|block| block.instructions.iter().all(|instruction| {
                    !matches!(
                        instruction,
                        SIRInstruction::Load(_, addr, _, _)
                            | SIRInstruction::Store(addr, _, _, _, _, _)
                            if addr.region == WORKING_REGION
                    ) && !matches!(instruction, SIRInstruction::Commit(..))
                }))
        );
    }
}
