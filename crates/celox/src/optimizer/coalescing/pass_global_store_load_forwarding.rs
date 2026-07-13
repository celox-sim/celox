//! Memory-SSA based store-to-load forwarding for combinational state.
//!
//! The local forwarding pass cannot see through branches or recovered loops.
//! This pass constructs pruned SSA names for exact, non-aliased static memory
//! slots and replaces dominated loads with the reaching stored value.  Stores
//! remain in place: observable state, trigger timing, and register pressure do
//! not depend on a later exit-store materialization policy.

use std::collections::VecDeque;

use super::pass_manager::ExecutionUnitPass;
use super::shared::{batch_replace_in_inst, batch_replace_in_terminator};
use crate::ir::*;
use crate::optimizer::PassOptions;
use crate::{HashMap, HashSet};

pub(super) struct GlobalStoreLoadForwardingPass;

impl ExecutionUnitPass for GlobalStoreLoadForwardingPass {
    fn name(&self) -> &'static str {
        "global_store_load_forwarding"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        forward_global_static_slots(eu);
    }
}

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
                    if addr.region == STABLE_REGION =>
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
                SIRInstruction::Store(addr, SIROffset::Static(bit_offset), width, source, _, _)
                    if addr.region == STABLE_REGION =>
                {
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
                    address_facts.def_blocks.insert(block_id);
                    defined.insert(*addr);
                }
                SIRInstruction::Load(_, addr, _, _)
                | SIRInstruction::Store(addr, _, _, _, _, _)
                    if addr.region == STABLE_REGION =>
                {
                    facts.entry(*addr).or_default().invalid = true;
                }
                SIRInstruction::Commit(source, destination, ..) => {
                    if source.region == STABLE_REGION {
                        facts.entry(*source).or_default().invalid = true;
                    }
                    if destination.region == STABLE_REGION {
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

fn forward_global_static_slots(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) -> bool {
    let Some(cfg) = Cfg::new(eu) else {
        return false;
    };
    let facts = collect_address_facts(eu);
    let mut candidates = facts
        .into_values()
        .filter(|facts| !facts.invalid && facts.has_load && facts.has_store)
        .filter_map(|facts| {
            let key = facts.key?;
            let phi_blocks = phi_blocks_for_slot(&cfg, &facts);
            let ty = facts.ty?;
            (!phi_blocks.contains(&0)).then_some(SlotPlan {
                key,
                ty,
                phi_blocks,
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

    enum Visit {
        Enter(usize),
        Exit(Vec<usize>),
    }
    let mut values = vec![Vec::<RegisterId>::new(); candidates.len()];
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
                    values[slot].push(register);
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
                            if let Some(&slot) = slot_index.get(&key) {
                                if let Some(&value) = values[slot].last() {
                                    aliases.insert(destination, value);
                                    changed = true;
                                } else {
                                    values[slot].push(destination);
                                    pushed_slots.push(slot);
                                    instructions.push(SIRInstruction::Load(
                                        destination,
                                        addr,
                                        SIROffset::Static(bit_offset),
                                        width,
                                    ));
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
                                values[slot].push(source);
                                pushed_slots.push(slot);
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
                        let value = if let Some(&value) = values[slot].last() {
                            value
                        } else {
                            let candidate = &candidates[slot];
                            let register = alloc_register(
                                &mut eu.register_map,
                                &mut next_register,
                                &candidate.ty,
                            );
                            instructions.push(SIRInstruction::Load(
                                register,
                                candidate.key.addr,
                                SIROffset::Static(candidate.key.bit_offset),
                                candidate.key.width,
                            ));
                            values[slot].push(register);
                            pushed_slots.push(slot);
                            register
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
        RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: InstanceId(0),
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
}
