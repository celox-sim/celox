//! Memory SSA and mem2reg for exact scalar SIR memory slots.
//!
//! The local forwarding pass cannot see through branches or recovered loops.
//! This pass constructs pruned SSA names for exact, non-aliased static memory
//! slots and replaces dominated loads with the reaching stored value. Observable
//! slots retain their stores. A whole-program entry point additionally promotes
//! non-escaping, definitely-defined combinational slots and removes their stores.

use std::collections::VecDeque;

use super::shared::{batch_replace_in_inst, batch_replace_in_terminator};
use crate::ir::*;
use crate::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct SlotKey {
    addr: RegionedAbsoluteAddr,
    bit_offset: usize,
    width: usize,
}

#[derive(Default)]
struct AddressFacts {
    key: Option<SlotKey>,
    ty: Option<RegisterType>,
    invalid: bool,
    has_load: bool,
    has_store: bool,
    has_effectful_store: bool,
    def_blocks: HashSet<BlockId>,
    upward_use_blocks: HashSet<BlockId>,
}

impl AddressFacts {
    fn record_access(&mut self, key: SlotKey, ty: &RegisterType) {
        if self.key.is_some_and(|previous| previous != key)
            || self.ty.as_ref().is_some_and(|previous| previous != ty)
            || ty.width() != key.width
            || key.width == 0
        {
            self.invalid = true;
        }
        self.key.get_or_insert(key);
        self.ty.get_or_insert_with(|| ty.clone());
    }
}

struct SlotPlan {
    key: SlotKey,
    ty: RegisterType,
    phi_blocks: Vec<usize>,
    promote: bool,
}

struct Cfg {
    block_ids: Vec<BlockId>,
    index: HashMap<BlockId, usize>,
    predecessors: Vec<Vec<usize>>,
    successors: Vec<Vec<usize>>,
    dom_children: Vec<Vec<usize>>,
    dominance_frontier: Vec<Vec<usize>>,
}

impl Cfg {
    fn new(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> Option<Self> {
        fn visit(
            eu: &ExecutionUnit<RegionedAbsoluteAddr>,
            block_id: BlockId,
            seen: &mut HashSet<BlockId>,
            postorder: &mut Vec<BlockId>,
        ) {
            if !seen.insert(block_id) {
                return;
            }
            let mut successors = terminator_successors(&eu.blocks[&block_id].terminator);
            successors.sort_unstable();
            for successor in successors {
                visit(eu, successor, seen, postorder);
            }
            postorder.push(block_id);
        }

        let mut seen = HashSet::default();
        let mut block_ids = Vec::new();
        visit(eu, eu.entry_block_id, &mut seen, &mut block_ids);
        if seen.len() != eu.blocks.len() {
            return None;
        }
        block_ids.reverse();
        let index = block_ids
            .iter()
            .copied()
            .enumerate()
            .map(|(index, block)| (block, index))
            .collect::<HashMap<_, _>>();
        let mut successors = vec![Vec::new(); block_ids.len()];
        let mut predecessors = vec![Vec::new(); block_ids.len()];
        for (block_index, block_id) in block_ids.iter().copied().enumerate() {
            for successor in terminator_successors(&eu.blocks[&block_id].terminator) {
                let &successor_index = index.get(&successor)?;
                successors[block_index].push(successor_index);
                predecessors[successor_index].push(block_index);
            }
        }
        for edges in successors.iter_mut().chain(&mut predecessors) {
            edges.sort_unstable();
            edges.dedup();
        }

        let mut immediate_dominator = vec![None; block_ids.len()];
        immediate_dominator[0] = Some(0);
        let mut changed = true;
        while changed {
            changed = false;
            for block in 1..block_ids.len() {
                let mut defined_predecessors = predecessors[block]
                    .iter()
                    .copied()
                    .filter(|predecessor| immediate_dominator[*predecessor].is_some());
                let Some(mut new_idom) = defined_predecessors.next() else {
                    continue;
                };
                for predecessor in defined_predecessors {
                    new_idom = intersect_dominators(predecessor, new_idom, &immediate_dominator);
                }
                if immediate_dominator[block] != Some(new_idom) {
                    immediate_dominator[block] = Some(new_idom);
                    changed = true;
                }
            }
        }
        if immediate_dominator.iter().any(Option::is_none) {
            return None;
        }

        let mut dom_children = vec![Vec::new(); block_ids.len()];
        for block in 1..block_ids.len() {
            dom_children[immediate_dominator[block]?].push(block);
        }
        for children in &mut dom_children {
            children.sort_unstable();
        }

        let mut dominance_frontier = vec![HashSet::default(); block_ids.len()];
        for block in 0..block_ids.len() {
            if predecessors[block].len() < 2 {
                continue;
            }
            let idom = immediate_dominator[block]?;
            for &predecessor in &predecessors[block] {
                let mut runner = predecessor;
                while runner != idom {
                    dominance_frontier[runner].insert(block);
                    let next = immediate_dominator[runner]?;
                    if next == runner {
                        return None;
                    }
                    runner = next;
                }
            }
        }
        let mut dominance_frontier = dominance_frontier
            .into_iter()
            .map(|frontier| {
                let mut frontier = frontier.into_iter().collect::<Vec<_>>();
                frontier.sort_unstable();
                frontier
            })
            .collect::<Vec<_>>();
        for frontier in &mut dominance_frontier {
            frontier.dedup();
        }

        Some(Self {
            block_ids,
            index,
            predecessors,
            successors,
            dom_children,
            dominance_frontier,
        })
    }
}

fn terminator_successors(terminator: &SIRTerminator) -> Vec<BlockId> {
    match terminator {
        SIRTerminator::Jump(target, _) => vec![*target],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![true_block.0, false_block.0],
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

fn intersect_dominators(
    mut left: usize,
    mut right: usize,
    immediate_dominator: &[Option<usize>],
) -> usize {
    while left != right {
        while left > right {
            left = immediate_dominator[left].expect("dominator predecessor is defined");
        }
        while right > left {
            right = immediate_dominator[right].expect("dominator predecessor is defined");
        }
    }
    left
}

fn collect_address_facts(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    region: u32,
) -> HashMap<RegionedAbsoluteAddr, AddressFacts> {
    let mut facts = HashMap::<RegionedAbsoluteAddr, AddressFacts>::default();
    let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
    block_ids.sort_unstable();
    for block_id in block_ids {
        let block = &eu.blocks[&block_id];
        let mut defined = HashSet::default();
        for instruction in &block.instructions {
            match instruction {
                SIRInstruction::Load(dst, addr, SIROffset::Static(bit_offset), width)
                    if addr.region == region =>
                {
                    let address_facts = facts.entry(*addr).or_default();
                    address_facts.record_access(
                        SlotKey {
                            addr: *addr,
                            bit_offset: *bit_offset,
                            width: *width,
                        },
                        &eu.register_map[dst],
                    );
                    address_facts.has_load = true;
                    if !defined.contains(addr) {
                        address_facts.upward_use_blocks.insert(block_id);
                    }
                }
                SIRInstruction::Store(
                    addr,
                    SIROffset::Static(bit_offset),
                    width,
                    source,
                    triggers,
                    capture_sites,
                ) if addr.region == region => {
                    let address_facts = facts.entry(*addr).or_default();
                    address_facts.record_access(
                        SlotKey {
                            addr: *addr,
                            bit_offset: *bit_offset,
                            width: *width,
                        },
                        &eu.register_map[source],
                    );
                    address_facts.has_store = true;
                    address_facts.has_effectful_store |=
                        !triggers.is_empty() || !capture_sites.is_empty();
                    address_facts.def_blocks.insert(block_id);
                    defined.insert(*addr);
                }
                SIRInstruction::Load(_, addr, _, _)
                | SIRInstruction::Store(addr, _, _, _, _, _)
                    if addr.region == region =>
                {
                    facts.entry(*addr).or_default().invalid = true;
                }
                SIRInstruction::Commit(source, destination, ..) => {
                    if source.region == region {
                        facts.entry(*source).or_default().invalid = true;
                    }
                    if destination.region == region {
                        facts.entry(*destination).or_default().invalid = true;
                    }
                }
                _ => {}
            }
        }
    }
    facts
}

fn live_in_blocks(
    cfg: &Cfg,
    def_blocks: &HashSet<BlockId>,
    upward_use_blocks: &HashSet<BlockId>,
) -> Vec<bool> {
    let mut definitions = vec![false; cfg.block_ids.len()];
    for block in def_blocks {
        definitions[cfg.index[block]] = true;
    }
    let mut live_in = vec![false; cfg.block_ids.len()];
    let mut work = VecDeque::new();
    for block in upward_use_blocks {
        let index = cfg.index[block];
        if !live_in[index] {
            live_in[index] = true;
            work.push_back(index);
        }
    }
    while let Some(block) = work.pop_front() {
        for &predecessor in &cfg.predecessors[block] {
            if !definitions[predecessor] && !live_in[predecessor] {
                live_in[predecessor] = true;
                work.push_back(predecessor);
            }
        }
    }
    live_in
}

fn phi_blocks_for_slot(cfg: &Cfg, facts: &AddressFacts) -> Vec<usize> {
    let live_in = live_in_blocks(cfg, &facts.def_blocks, &facts.upward_use_blocks);
    let definition_indices = facts
        .def_blocks
        .iter()
        .map(|block| cfg.index[block])
        .collect::<HashSet<_>>();
    let mut phi_blocks = HashSet::default();
    let mut queued = definition_indices.clone();
    let mut work = definition_indices.iter().copied().collect::<Vec<_>>();
    while let Some(definition) = work.pop() {
        for &frontier in &cfg.dominance_frontier[definition] {
            if !live_in[frontier] || !phi_blocks.insert(frontier) {
                continue;
            }
            if queued.insert(frontier) {
                work.push(frontier);
            }
        }
    }
    let mut phi_blocks = phi_blocks.into_iter().collect::<Vec<_>>();
    phi_blocks.sort_unstable();
    phi_blocks
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
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

#[cfg(test)]
fn forward_global_static_slots(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    let no_promotions = HashSet::default();
    rewrite_global_static_slots(
        eu,
        STABLE_REGION,
        PromotionPolicy::Exact(&no_promotions),
        &HashMap::default(),
    )
}

#[derive(Clone, Copy)]
enum PromotionPolicy<'a> {
    Exact(&'a HashSet<SlotKey>),
}

fn rewrite_global_static_slots(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    region: u32,
    promotion: PromotionPolicy<'_>,
    fallback_definitions: &HashMap<RegisterId, SlotKey>,
) -> bool {
    let Some(cfg) = Cfg::new(eu) else {
        return false;
    };
    let facts = collect_address_facts(eu, region);
    let mut candidates = facts
        .into_values()
        .filter(|facts| !facts.invalid && facts.has_load && facts.has_store)
        .filter_map(|facts| {
            let key = facts.key?;
            let phi_blocks = phi_blocks_for_slot(&cfg, &facts);
            let live_in = live_in_blocks(&cfg, &facts.def_blocks, &facts.upward_use_blocks);
            let selected_for_promotion = match promotion {
                PromotionPolicy::Exact(slots) => slots.contains(&key),
            };
            let promote = selected_for_promotion && !facts.has_effectful_store && !live_in[0];
            let ty = facts.ty?;
            (!phi_blocks.contains(&0)).then_some(SlotPlan {
                key,
                ty,
                phi_blocks,
                promote,
            })
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| candidate.key);
    if candidates.is_empty() {
        return false;
    }

    let slot_index = candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| (candidate.key, index))
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
    enum ReachingValue {
        Register(RegisterId),
        StableFallback,
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
                for mut instruction in old_instructions {
                    batch_replace_in_inst(&mut instruction, &aliases);
                    match instruction {
                        SIRInstruction::Load(
                            destination,
                            addr,
                            SIROffset::Static(bit_offset),
                            width,
                        ) => {
                            let key = SlotKey {
                                addr,
                                bit_offset,
                                width,
                            };
                            if let Some(seed_key) = fallback_definitions.get(&destination)
                                && let Some(&slot) = slot_index.get(seed_key)
                                && candidates[slot].promote
                            {
                                changed = true;
                                continue;
                            }
                            if let Some(&slot) = slot_index.get(&key) {
                                match values[slot].last().copied() {
                                    Some(ReachingValue::Register(value)) => {
                                        aliases.insert(destination, value);
                                        changed = true;
                                    }
                                    Some(ReachingValue::StableFallback) => {
                                        let mut stable = addr;
                                        stable.region = STABLE_REGION;
                                        values[slot].push(ReachingValue::Register(destination));
                                        pushed_slots.push(slot);
                                        instructions.push(SIRInstruction::Load(
                                            destination,
                                            stable,
                                            SIROffset::Static(bit_offset),
                                            width,
                                        ));
                                    }
                                    None => {
                                        values[slot].push(ReachingValue::Register(destination));
                                        pushed_slots.push(slot);
                                        instructions.push(SIRInstruction::Load(
                                            destination,
                                            addr,
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
                            let key = SlotKey {
                                addr,
                                bit_offset,
                                width,
                            };
                            if let Some(&slot) = slot_index.get(&key) {
                                let value = if candidates[slot].promote
                                    && fallback_definitions.get(&source) == Some(&key)
                                {
                                    ReachingValue::StableFallback
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
                                let mut address = candidate.key.addr;
                                if matches!(current, Some(ReachingValue::StableFallback)) {
                                    address.region = STABLE_REGION;
                                }
                                instructions.push(SIRInstruction::Load(
                                    register,
                                    address,
                                    SIROffset::Static(candidate.key.bit_offset),
                                    candidate.key.width,
                                ));
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
    changed
}

#[derive(Default)]
struct WorkingRoundTripFacts {
    key: Option<SlotKey>,
    ty: Option<RegisterType>,
    invalid: bool,
    has_seed: bool,
    has_store: bool,
    has_apply: bool,
}

impl WorkingRoundTripFacts {
    fn record_key(&mut self, key: SlotKey) {
        if self.key.is_some_and(|previous| previous != key) || key.width == 0 {
            self.invalid = true;
        }
        self.key.get_or_insert(key);
    }

    fn record_type(&mut self, ty: &RegisterType) {
        if self.ty.as_ref().is_some_and(|previous| previous != ty) {
            self.invalid = true;
        }
        self.ty.get_or_insert_with(|| ty.clone());
    }
}

/// Promote the ordinary WORKING-region round trip in a merged eval_apply_ff:
///
/// `Commit(STABLE→WORKING)` becomes the SSA live-in, WORKING stores become SSA
/// definitions, and `Commit(WORKING→STABLE)` becomes the sole writeback. This
/// is deliberately limited to one exact static scalar fragment per address;
/// sparse and dynamically addressed next-state storage keep their own lowering.
pub(crate) fn promote_eval_apply_working_round_trips(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
) -> bool {
    let mut facts = HashMap::<AbsoluteAddr, WorkingRoundTripFacts>::default();
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            match instruction {
                SIRInstruction::Load(destination, address, SIROffset::Static(offset), width)
                    if address.region == WORKING_REGION =>
                {
                    let key = SlotKey {
                        addr: *address,
                        bit_offset: *offset,
                        width: *width,
                    };
                    let entry = facts.entry(address.absolute_addr()).or_default();
                    entry.record_key(key);
                    entry.record_type(&eu.register_map[destination]);
                }
                SIRInstruction::Store(
                    address,
                    SIROffset::Static(offset),
                    width,
                    source,
                    triggers,
                    capture_sites,
                ) if address.region == WORKING_REGION => {
                    let key = SlotKey {
                        addr: *address,
                        bit_offset: *offset,
                        width: *width,
                    };
                    let entry = facts.entry(address.absolute_addr()).or_default();
                    entry.record_key(key);
                    entry.record_type(&eu.register_map[source]);
                    entry.has_store = true;
                    entry.invalid |= !triggers.is_empty() || !capture_sites.is_empty();
                }
                SIRInstruction::Commit(
                    source,
                    destination,
                    SIROffset::Static(offset),
                    width,
                    _,
                ) if source.region == STABLE_REGION
                    && destination.region == WORKING_REGION
                    && source.absolute_addr() == destination.absolute_addr() =>
                {
                    let key = SlotKey {
                        addr: *destination,
                        bit_offset: *offset,
                        width: *width,
                    };
                    let entry = facts.entry(destination.absolute_addr()).or_default();
                    entry.record_key(key);
                    entry.has_seed = true;
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
                    let key = SlotKey {
                        addr: *source,
                        bit_offset: *offset,
                        width: *width,
                    };
                    let entry = facts.entry(source.absolute_addr()).or_default();
                    entry.record_key(key);
                    entry.has_apply = true;
                }
                SIRInstruction::Load(_, address, _, _)
                | SIRInstruction::Store(address, _, _, _, _, _)
                    if address.region == WORKING_REGION =>
                {
                    facts.entry(address.absolute_addr()).or_default().invalid = true;
                }
                SIRInstruction::Commit(source, destination, _, _, _)
                    if source.region == WORKING_REGION || destination.region == WORKING_REGION =>
                {
                    facts
                        .entry(if source.region == WORKING_REGION {
                            source.absolute_addr()
                        } else {
                            destination.absolute_addr()
                        })
                        .or_default()
                        .invalid = true;
                }
                _ => {}
            }
        }
    }

    let eligible = facts
        .into_values()
        .filter(|facts| !facts.invalid && facts.has_seed && facts.has_store && facts.has_apply)
        .filter_map(|facts| Some((facts.key?, facts.ty?)))
        .collect::<HashMap<_, _>>();
    if eligible.is_empty() {
        return false;
    }

    let mut next_register = eu
        .register_map
        .keys()
        .map(|register| register.0)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let mut fallback_definitions = HashMap::default();
    for block in eu.blocks.values_mut() {
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
                    && eligible.contains_key(&SlotKey {
                        addr: destination,
                        bit_offset: offset,
                        width,
                    }) =>
                {
                    let key = SlotKey {
                        addr: destination,
                        bit_offset: offset,
                        width,
                    };
                    let register =
                        alloc_register(&mut eu.register_map, &mut next_register, &eligible[&key]);
                    fallback_definitions.insert(register, key);
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
                    && eligible.contains_key(&SlotKey {
                        addr: source,
                        bit_offset: offset,
                        width,
                    }) =>
                {
                    let key = SlotKey {
                        addr: source,
                        bit_offset: offset,
                        width,
                    };
                    let register =
                        alloc_register(&mut eu.register_map, &mut next_register, &eligible[&key]);
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

    let slots = eligible.keys().copied().collect::<HashSet<_>>();
    let changed = rewrite_global_static_slots(
        eu,
        WORKING_REGION,
        PromotionPolicy::Exact(&slots),
        &fallback_definitions,
    );
    if changed {
        sink_phi_writebacks_to_predecessors(eu);
    }
    changed
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
            SIROffset::Static(other_offset) => {
                ranges_overlap(offset, width, *other_offset, other_width)
            }
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
fn sink_phi_writebacks_to_predecessors(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    struct Candidate {
        instruction: usize,
        parameter: usize,
        register: RegisterId,
        edge_stores: Vec<(BlockId, SIRInstruction<RegionedAbsoluteAddr>)>,
    }

    let mut changed = false;
    while let Some(cfg) = Cfg::new(eu) {
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
                    edge_stores.push((
                        predecessor_id,
                        SIRInstruction::Store(
                            *address,
                            SIROffset::Static(*offset),
                            *width,
                            incoming,
                            triggers.clone(),
                            capture_sites.clone(),
                        ),
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
                eu.blocks
                    .get_mut(predecessor)
                    .unwrap()
                    .instructions
                    .push(store.clone());
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

        assert!(forward_global_static_slots(&mut eu));
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

        assert!(forward_global_static_slots(&mut eu));
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

        assert!(forward_global_static_slots(&mut eu));
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

        assert!(!forward_global_static_slots(&mut eu));
        eu.verify_result().unwrap();
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
        assert!(matches!(
            eu.blocks[&BlockId(2)].instructions.as_slice(),
            [
                SIRInstruction::Load(source, load_address, SIROffset::Static(0), 8),
                SIRInstruction::Store(store_address, SIROffset::Static(0), 8, stored, _, _),
            ] if *load_address == stable && *store_address == stable && source == stored
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
        assert!(!sink_phi_writebacks_to_predecessors(&mut eu));
        eu.verify_result().unwrap();
    }
}
