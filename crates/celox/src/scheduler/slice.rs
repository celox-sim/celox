//! Parts of a combinational settle that compute chosen storage.
//!
//! The settle's units are indexed once: what each register and each store
//! depends on, through data and control. A slice is then a walk from the
//! storage it must compute, linear in what it reaches.
use super::*;
use celox_sir::BlockId;

/// What a slice needs.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Need {
    Storage(AbsoluteAddr),
    Register(usize, RegisterId),
    /// The block runs as in the settle: what decides that it runs.
    Block(usize, BlockId),
}

/// Where a register is defined.
#[derive(Clone, Copy)]
enum Definition {
    Instruction(BlockId, usize),
    /// A block parameter, with its index.
    Parameter(BlockId, usize),
}

struct Indexed {
    definitions: HashMap<RegisterId, Definition>,
    /// The predecessors of each block, with the arguments of their edge.
    incoming: HashMap<BlockId, Vec<(BlockId, Vec<RegisterId>)>>,
    /// Immediate controlling branch/switch blocks and their selectors.
    control: HashMap<BlockId, Vec<(BlockId, RegisterId)>>,
}

pub(crate) struct CombSlicer<'a> {
    units: &'a [Unit],
    storage: HashMap<AbsoluteAddr, AbsoluteAddr>,
    indexed: Vec<Indexed>,
    /// The stores of each storage representative: unit, block, instruction.
    stores: HashMap<AbsoluteAddr, Vec<(usize, BlockId, usize)>>,
}

fn successors(terminator: &SIRTerminator) -> Vec<(BlockId, &[RegisterId])> {
    match terminator {
        SIRTerminator::Jump(target, arguments) => vec![(*target, arguments.as_slice())],
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => vec![
            (true_block.0, true_block.1.as_slice()),
            (false_block.0, false_block.1.as_slice()),
        ],
        SIRTerminator::Switch { cases, default, .. } => cases
            .iter()
            .map(|case| (case.target, &[][..]))
            .chain([(*default, &[][..])])
            .collect(),
        SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
    }
}

fn selector(terminator: &SIRTerminator) -> Option<RegisterId> {
    match terminator {
        SIRTerminator::Branch { cond, .. } => Some(*cond),
        SIRTerminator::Switch { selector, .. } => Some(*selector),
        _ => None,
    }
}

/// The immediate post-dominator of each block reachable from the entry,
/// `None` for the virtual exit (Cooper, Harvey and Kennedy).
fn post_dominators(unit: &Unit) -> HashMap<BlockId, Option<BlockId>> {
    let mut predecessors: HashMap<BlockId, Vec<BlockId>> = HashMap::default();
    let mut exits = Vec::new();
    for (&id, block) in &unit.blocks {
        let next = successors(&block.terminator);
        if next.is_empty() {
            exits.push(id);
        }
        for (target, _) in next {
            predecessors.entry(target).or_default().push(id);
        }
    }
    exits.sort_unstable();
    // Postorder over the reverse graph, starting at the virtual exit.
    let mut order: Vec<BlockId> = Vec::new();
    let mut visited: HashSet<BlockId> = HashSet::default();
    for &exit in &exits {
        if !visited.insert(exit) {
            continue;
        }
        let mut stack = vec![(exit, 0usize)];
        while let Some((block, next)) = stack.pop() {
            let preds = predecessors.get(&block).map_or(&[][..], Vec::as_slice);
            if next < preds.len() {
                stack.push((block, next + 1));
                let pred = preds[next];
                if visited.insert(pred) {
                    stack.push((pred, 0));
                }
            } else {
                order.push(block);
            }
        }
    }
    // Number blocks by postorder; the virtual exit is above all.
    let number: HashMap<BlockId, usize> = order
        .iter()
        .enumerate()
        .map(|(index, &block)| (block, index))
        .collect();
    let exit = order.len();
    let mut ipdom: HashMap<BlockId, usize> = HashMap::default();
    for &block in &exits {
        ipdom.insert(block, exit);
    }
    let intersect = |ipdom: &HashMap<BlockId, usize>, mut a: usize, mut b: usize| {
        while a != b {
            while a < b {
                a = ipdom[&order[a]];
            }
            while b < a {
                b = ipdom[&order[b]];
            }
        }
        a
    };
    let mut changed = true;
    while changed {
        changed = false;
        for &block in order.iter().rev() {
            if exits.contains(&block) {
                continue;
            }
            let mut new: Option<usize> = None;
            for (target, _) in successors(&unit.blocks[&block].terminator) {
                let Some(&known) = ipdom.get(&target).map(|_| &number[&target]) else {
                    continue;
                };
                new = Some(match new {
                    None => known,
                    Some(current) => intersect(&ipdom, current, known),
                });
            }
            if let Some(new) = new
                && ipdom.get(&block) != Some(&new)
            {
                ipdom.insert(block, new);
                changed = true;
            }
        }
    }
    ipdom
        .into_iter()
        .map(|(block, dominator)| (block, (dominator != exit).then(|| order[dominator])))
        .collect()
}

fn index(unit: &Unit) -> Indexed {
    let mut definitions = HashMap::default();
    let mut incoming: HashMap<BlockId, Vec<(BlockId, Vec<RegisterId>)>> = HashMap::default();
    for (&id, block) in &unit.blocks {
        for (position, &parameter) in block.params.iter().enumerate() {
            definitions.insert(parameter, Definition::Parameter(id, position));
        }
        for (position, inst) in block.instructions.iter().enumerate() {
            if let Some(register) = inst.defined_register() {
                definitions.insert(register, Definition::Instruction(id, position));
            }
        }
        for (target, arguments) in successors(&block.terminator) {
            incoming
                .entry(target)
                .or_default()
                .push((id, arguments.to_vec()));
        }
    }
    // A block is control dependent on a branch when it lies on a path from
    // one of the branch's successors up to (not including) the branch's
    // post-dominator.
    let ipdom = post_dominators(unit);
    let mut control: HashMap<BlockId, Vec<(BlockId, RegisterId)>> = HashMap::default();
    for (&id, block) in &unit.blocks {
        let Some(selector) = selector(&block.terminator) else {
            continue;
        };
        // A block that cannot reach an exit has no post-dominator: treat
        // everything it reaches as depending on it.
        let stop = ipdom.get(&id).copied().flatten();
        for (target, _) in successors(&block.terminator) {
            let mut runner = Some(target);
            let mut seen = HashSet::default();
            while let Some(current) = runner {
                if Some(current) == stop || !seen.insert(current) {
                    break;
                }
                control.entry(current).or_default().push((id, selector));
                runner = ipdom.get(&current).copied().flatten();
            }
        }
    }
    Indexed {
        definitions,
        incoming,
        control,
    }
}

impl<'a> CombSlicer<'a> {
    pub(crate) fn new(units: &'a [Unit], storage: HashMap<AbsoluteAddr, AbsoluteAddr>) -> Self {
        let mut stores: HashMap<AbsoluteAddr, Vec<(usize, BlockId, usize)>> = HashMap::default();
        let representative =
            |address: AbsoluteAddr| storage.get(&address).copied().unwrap_or(address);
        for (unit_index, unit) in units.iter().enumerate() {
            for (&id, block) in &unit.blocks {
                for (position, inst) in block.instructions.iter().enumerate() {
                    if let Some(write) = inst.memory_write() {
                        stores
                            .entry(representative(write.addr.absolute_addr()))
                            .or_default()
                            .push((unit_index, id, position));
                    }
                }
            }
        }
        Self {
            units,
            indexed: units.iter().map(index).collect(),
            stores,
            storage,
        }
    }

    fn representative(&self, address: AbsoluteAddr) -> AbsoluteAddr {
        self.storage.get(&address).copied().unwrap_or(address)
    }

    /// Whether some unit stores `address`.
    pub(crate) fn stores(&self, address: AbsoluteAddr) -> bool {
        self.stores.contains_key(&self.representative(address))
    }

    /// Storage which can change after the supplied inputs change, through
    /// both data and control dependencies, including block-argument choices.
    pub(crate) fn affected_storage(
        &self,
        inputs: impl IntoIterator<Item = AbsoluteAddr>,
    ) -> HashSet<AbsoluteAddr> {
        let mut users: HashMap<Need, Vec<Need>> = HashMap::default();
        let mut edge = |source, target| users.entry(source).or_default().push(target);
        for (unit, body) in self.units.iter().enumerate() {
            for (&block, body) in &body.blocks {
                let block_need = Need::Block(unit, block);
                for &(controller, selector) in
                    self.indexed[unit].control.get(&block).into_iter().flatten()
                {
                    edge(Need::Register(unit, selector), block_need);
                    // The selector may be captured in the entry block. A
                    // nested branch still executes only when its controlling
                    // block executes, even when that selector is unchanged.
                    edge(Need::Block(unit, controller), block_need);
                }
                for (position, &parameter) in body.params.iter().enumerate() {
                    let target = Need::Register(unit, parameter);
                    edge(block_need, target);
                    for (predecessor, arguments) in self.indexed[unit]
                        .incoming
                        .get(&block)
                        .into_iter()
                        .flatten()
                    {
                        if let Some(&argument) = arguments.get(position) {
                            edge(Need::Register(unit, argument), target);
                        }
                        edge(Need::Block(unit, *predecessor), target);
                        // A branch may pass different constants directly to
                        // the same successor: neither argument itself changes.
                        if let Some(selector) =
                            selector(&self.units[unit].blocks[predecessor].terminator)
                        {
                            edge(Need::Register(unit, selector), target);
                        }
                    }
                }
                for instruction in &body.instructions {
                    let register = instruction
                        .defined_register()
                        .map(|register| Need::Register(unit, register));
                    let storage = instruction.memory_write().map(|write| {
                        Need::Storage(self.representative(write.addr.absolute_addr()))
                    });
                    for target in register.into_iter().chain(storage) {
                        edge(block_need, target);
                        instruction
                            .for_each_use(|register| edge(Need::Register(unit, register), target));
                        if let Some(read) = instruction.memory_read() {
                            edge(
                                Need::Storage(self.representative(read.addr.absolute_addr())),
                                target,
                            );
                        }
                    }
                }
            }
        }
        let mut work: Vec<_> = inputs
            .into_iter()
            .map(|address| Need::Storage(self.representative(address)))
            .collect();
        let mut seen = HashSet::default();
        while let Some(need) = work.pop() {
            if seen.insert(need) {
                work.extend(users.get(&need).into_iter().flatten().copied());
            }
        }
        seen.into_iter()
            .filter_map(|need| match need {
                Need::Storage(address) if self.stores.contains_key(&address) => Some(address),
                _ => None,
            })
            .collect()
    }

    /// Re-evaluate changed inputs and stateful activations, reading cached
    /// values of unaffected intermediate storage instead of recomputing it.
    pub(crate) fn affected_slice(
        &self,
        inputs: impl IntoIterator<Item = AbsoluteAddr>,
    ) -> Vec<Unit> {
        let affected = self.affected_storage(inputs);
        self.slice_selected(affected.iter().copied(), true, Some(&affected))
    }

    /// The part of the settle that computes `reads`: every store of storage
    /// they or the part load, and what decides it. Stores the part does not
    /// need, and the settle's other effects, are left to the settle.
    pub(crate) fn slice(&self, reads: impl IntoIterator<Item = AbsoluteAddr>) -> Vec<Unit> {
        self.slice_with_control(reads, false)
    }

    /// Even without a mutable-state sink, an eager activation must retain
    /// control flow which cannot fold away, such as a nonterminating loop.
    pub(crate) fn eager_slice(&self, reads: impl IntoIterator<Item = AbsoluteAddr>) -> Vec<Unit> {
        self.slice_with_control(reads, true)
    }

    fn slice_with_control(
        &self,
        reads: impl IntoIterator<Item = AbsoluteAddr>,
        preserve_control: bool,
    ) -> Vec<Unit> {
        self.slice_selected(reads, preserve_control, None)
    }

    fn slice_selected(
        &self,
        reads: impl IntoIterator<Item = AbsoluteAddr>,
        preserve_control: bool,
        allowed: Option<&HashSet<AbsoluteAddr>>,
    ) -> Vec<Unit> {
        let mut kept: Vec<HashSet<(BlockId, usize)>> = vec![HashSet::default(); self.units.len()];
        let mut seen: HashSet<Need> = HashSet::default();
        let mut work: Vec<Need> = reads
            .into_iter()
            .map(|address| self.representative(address))
            .filter(|address| self.stores.contains_key(address))
            .map(Need::Storage)
            .collect();
        // Reads no unit stores hold their values already. Built from no
        // needs, the part would still keep the loops it cannot fold.
        if work.is_empty() && !preserve_control {
            return Vec::new();
        }
        let mut units: Vec<Unit>;
        loop {
            self.walk(&mut work, &mut seen, &mut kept, allowed);
            units = self.build(&kept, &seen);
            // A terminator whose operands the walk did not reach survives
            // only where folding could not remove it: keep what it uses.
            for (unit_index, unit) in units.iter().enumerate() {
                for block in unit.blocks.values() {
                    block.terminator.for_each_use(|register| {
                        let need = Need::Register(unit_index, register);
                        if !seen.contains(&need) {
                            work.push(need);
                        }
                    });
                }
            }
            if work.is_empty() {
                return units;
            }
        }
    }

    fn walk(
        &self,
        work: &mut Vec<Need>,
        seen: &mut HashSet<Need>,
        kept: &mut [HashSet<(BlockId, usize)>],
        allowed: Option<&HashSet<AbsoluteAddr>>,
    ) {
        while let Some(need) = work.pop() {
            if !seen.insert(need) {
                continue;
            }
            match need {
                Need::Storage(address) => {
                    if allowed.is_some_and(|allowed| !allowed.contains(&address)) {
                        continue;
                    }
                    for &(unit, block, position) in self.stores.get(&address).into_iter().flatten()
                    {
                        kept[unit].insert((block, position));
                        let inst = &self.units[unit].blocks[&block].instructions[position];
                        inst.for_each_use(|register| work.push(Need::Register(unit, register)));
                        if let Some(read) = inst.memory_read() {
                            work.push(Need::Storage(
                                self.representative(read.addr.absolute_addr()),
                            ));
                        }
                        work.push(Need::Block(unit, block));
                    }
                }
                Need::Register(unit, register) => {
                    match self.indexed[unit].definitions.get(&register) {
                        Some(&Definition::Instruction(block, position)) => {
                            kept[unit].insert((block, position));
                            let inst = &self.units[unit].blocks[&block].instructions[position];
                            inst.for_each_use(|register| work.push(Need::Register(unit, register)));
                            if let Some(read) = inst.memory_read() {
                                work.push(Need::Storage(
                                    self.representative(read.addr.absolute_addr()),
                                ));
                            }
                            work.push(Need::Block(unit, block));
                        }
                        Some(&Definition::Parameter(block, position)) => {
                            for (predecessor, arguments) in self.indexed[unit]
                                .incoming
                                .get(&block)
                                .into_iter()
                                .flatten()
                            {
                                if let Some(&argument) = arguments.get(position) {
                                    work.push(Need::Register(unit, argument));
                                }
                                work.push(Need::Block(unit, *predecessor));
                            }
                            work.push(Need::Block(unit, block));
                        }
                        None => {}
                    }
                }
                Need::Block(unit, block) => {
                    for &(controller, selector) in
                        self.indexed[unit].control.get(&block).into_iter().flatten()
                    {
                        work.push(Need::Register(unit, selector));
                        work.push(Need::Block(unit, controller));
                    }
                }
            }
        }
    }

    fn build(&self, kept: &[HashSet<(BlockId, usize)>], seen: &HashSet<Need>) -> Vec<Unit> {
        self.units
            .iter()
            .enumerate()
            .map(|(unit_index, unit)| {
                let mut unit = unit.clone();
                let needed =
                    |register: &RegisterId| seen.contains(&Need::Register(unit_index, *register));
                // Unneeded parameters go with their arguments.
                let dropped: HashMap<BlockId, Vec<bool>> = unit
                    .blocks
                    .iter()
                    .map(|(&id, block)| (id, block.params.iter().map(|p| !needed(p)).collect()))
                    .collect();
                for (&id, block) in unit.blocks.iter_mut() {
                    let mut position = 0;
                    block.instructions.retain(|_| {
                        let keep = kept[unit_index].contains(&(id, position));
                        position += 1;
                        keep
                    });
                    let mut index = 0;
                    block.params.retain(|_| {
                        let keep = !dropped[&id][index];
                        index += 1;
                        keep
                    });
                    let prune = |target: BlockId, arguments: &mut Vec<RegisterId>| {
                        let mut index = 0;
                        arguments.retain(|_| {
                            let keep = !dropped[&target][index];
                            index += 1;
                            keep
                        });
                    };
                    match &mut block.terminator {
                        SIRTerminator::Jump(target, arguments) => prune(*target, arguments),
                        SIRTerminator::Branch {
                            true_block,
                            false_block,
                            ..
                        } => {
                            prune(true_block.0, &mut true_block.1);
                            prune(false_block.0, &mut false_block.1);
                        }
                        _ => {}
                    }
                }
                fold_empty_branches(&mut unit);
                unit
            })
            .collect()
    }
}

/// Replace branches whose arms run nothing on the way to one block (with
/// the same arguments) by a jump there, and drop unreachable blocks.
fn fold_empty_branches(unit: &mut Unit) {
    // Where an empty, parameterless block chain ends, with its arguments.
    let forward = |unit: &Unit, mut block: BlockId| -> (BlockId, Vec<RegisterId>) {
        let mut arguments = Vec::new();
        let mut seen = HashSet::default();
        while seen.insert(block) {
            let current = &unit.blocks[&block];
            match &current.terminator {
                SIRTerminator::Jump(target, next)
                    if current.instructions.is_empty() && current.params.is_empty() =>
                {
                    arguments = next.clone();
                    block = *target;
                    if !arguments.is_empty() {
                        break;
                    }
                }
                _ => break,
            }
        }
        (block, arguments)
    };
    let mut changed = true;
    while changed {
        changed = false;
        let mut ids: Vec<_> = unit.blocks.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let targets: Vec<(BlockId, Vec<RegisterId>)> = match &unit.blocks[&id].terminator {
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } if true_block.1.is_empty() && false_block.1.is_empty() => {
                    vec![forward(unit, true_block.0), forward(unit, false_block.0)]
                }
                SIRTerminator::Switch { cases, default, .. } => cases
                    .iter()
                    .map(|case| forward(unit, case.target))
                    .chain([forward(unit, *default)])
                    .collect(),
                _ => continue,
            };
            if targets.windows(2).all(|pair| pair[0] == pair[1]) {
                let (target, arguments) = targets.into_iter().next().unwrap();
                unit.blocks.get_mut(&id).unwrap().terminator =
                    SIRTerminator::Jump(target, arguments);
                changed = true;
            }
        }
    }
    let mut reachable = HashSet::default();
    let mut work = vec![unit.entry_block_id];
    while let Some(id) = work.pop() {
        if reachable.insert(id) {
            work.extend(
                successors(&unit.blocks[&id].terminator)
                    .into_iter()
                    .map(|(t, _)| t),
            );
        }
    }
    unit.blocks.retain(|id, _| reachable.contains(id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox_design::{InstanceId, StateObjectId};
    use celox_sir::SIROffset;

    fn address(id: u32) -> AbsoluteAddr {
        AbsoluteAddr {
            instance_id: InstanceId(0),
            var_id: StateObjectId(id),
        }
    }

    fn stable(id: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, address(id))
    }

    #[test]
    fn eager_empty_sink_keeps_nonterminating_control() {
        let mut builder = Builder::new();
        let forever = builder.new_block();
        builder.seal_block(SIRTerminator::Jump(forever, Vec::new()));
        builder.switch_to_block(forever);
        builder.seal_block(SIRTerminator::Jump(forever, Vec::new()));
        let (blocks, register_map, _) = builder.drain();
        let units = [Unit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }];
        let slicer = CombSlicer::new(&units, HashMap::default());
        assert!(slicer.slice([]).is_empty());
        let eager = slicer.eager_slice([]);
        assert_eq!(eager.len(), 1);
        eager[0].verify();
        let cfg = celox_sir::cfg::SirCfg::analyze(&eager[0]).unwrap();
        assert!(
            !cfg.loops.is_empty(),
            "an activation cannot elide an infinite loop"
        );
    }

    /// `if (a) { x = c; y = c; }  z = c;`
    fn settle() -> Unit {
        let mut b = Builder::new();
        let [a, c, x, y, z] = [0, 1, 2, 3, 4];
        let cond = b.alloc_bit(1, false);
        let value = b.alloc_bit(8, false);
        b.emit(SIRInstruction::Load(
            cond,
            stable(a),
            SIROffset::Static(0),
            1,
        ));
        b.emit(SIRInstruction::Load(
            value,
            stable(c),
            SIROffset::Static(0),
            8,
        ));
        let then = b.new_block();
        let otherwise = b.new_block();
        let join = b.new_block();
        b.seal_block(SIRTerminator::Branch {
            cond,
            true_block: (then, Vec::new()),
            false_block: (otherwise, Vec::new()),
        });
        b.switch_to_block(then);
        for target in [x, y] {
            b.emit(SIRInstruction::Store(
                stable(target),
                SIROffset::Static(0),
                8,
                value,
                Vec::new(),
                Vec::new(),
            ));
        }
        b.seal_block(SIRTerminator::Jump(join, Vec::new()));
        b.switch_to_block(otherwise);
        b.seal_block(SIRTerminator::Jump(join, Vec::new()));
        b.switch_to_block(join);
        let again = b.alloc_bit(8, false);
        b.emit(SIRInstruction::Load(
            again,
            stable(c),
            SIROffset::Static(0),
            8,
        ));
        b.emit(SIRInstruction::Store(
            stable(z),
            SIROffset::Static(0),
            8,
            again,
            Vec::new(),
            Vec::new(),
        ));
        b.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = b.drain();
        Unit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    fn stored(units: &[Unit]) -> Vec<AbsoluteAddr> {
        let mut stored: Vec<_> = units
            .iter()
            .flat_map(instructions)
            .filter_map(|inst| inst.memory_write().map(|write| write.addr.absolute_addr()))
            .collect();
        stored.sort_unstable();
        stored
    }

    #[test]
    fn affected_slice_reads_cached_unaffected_intermediates() {
        let mut b = Builder::new();
        let a = b.alloc_bit(8, false);
        b.emit(SIRInstruction::Load(a, stable(0), SIROffset::Static(0), 8));
        b.emit(SIRInstruction::Store(
            stable(1),
            SIROffset::Static(0),
            8,
            a,
            vec![],
            vec![],
        ));
        let cached = b.alloc_bit(8, false);
        b.emit(SIRInstruction::Load(
            cached,
            stable(1),
            SIROffset::Static(0),
            8,
        ));
        let clock = b.alloc_bit(8, false);
        b.emit(SIRInstruction::Load(
            clock,
            stable(2),
            SIROffset::Static(0),
            8,
        ));
        let result = b.alloc_bit(8, false);
        b.emit(SIRInstruction::Binary(result, clock, BinaryOp::And, cached));
        b.emit(SIRInstruction::Store(
            stable(3),
            SIROffset::Static(0),
            8,
            result,
            vec![],
            vec![],
        ));
        b.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = b.drain();
        let units = [Unit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }];
        let slicer = CombSlicer::new(&units, HashMap::default());
        let slice = slicer.affected_slice([address(2)]);
        assert_eq!(stored(&slice), [address(3)]);
        assert!(instructions(&slice[0]).any(|inst| matches!(inst, SIRInstruction::Load(_, target, ..) if target.absolute_addr() == address(1))));
        assert!(!instructions(&slice[0]).any(|inst| matches!(inst, SIRInstruction::Load(_, target, ..) if target.absolute_addr() == address(0))));
        slice[0].verify();
    }

    #[test]
    fn affected_storage_tracks_a_branch_choosing_constant_block_arguments() {
        let mut b = Builder::new();
        let condition = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Load(
            condition,
            stable(0),
            SIROffset::Static(0),
            1,
        ));
        let zero = b.alloc_bit(8, false);
        let one = b.alloc_bit(8, false);
        b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
        let parameter = b.alloc_bit(8, false);
        let join = b.new_block_with(vec![parameter]);
        b.seal_block(SIRTerminator::Branch {
            cond: condition,
            true_block: (join, vec![one]),
            false_block: (join, vec![zero]),
        });
        b.switch_to_block(join);
        b.emit(SIRInstruction::Store(
            stable(1),
            SIROffset::Static(0),
            8,
            parameter,
            vec![],
            vec![],
        ));
        b.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = b.drain();
        let units = [Unit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }];
        let slicer = CombSlicer::new(&units, HashMap::default());
        assert_eq!(
            slicer.affected_storage([address(0)]),
            [address(1)].into_iter().collect()
        );
        let slice = slicer.affected_slice([address(0)]);
        slice[0].verify();
        assert_eq!(stored(&slice), [address(1)]);
    }

    #[test]
    fn affected_storage_follows_nested_branch_and_switch_execution() {
        // All selectors and constants are defined in the entry block, so
        // only block execution (not a new register definition) carries the
        // outer branch's influence into the nested stores.
        for switches in 0..8 {
            let mut b = Builder::new();
            let conditions: Vec<_> = (0..3)
                .map(|input| {
                    let register = b.alloc_bit(1, false);
                    b.emit(SIRInstruction::Load(
                        register,
                        stable(input),
                        SIROffset::Static(0),
                        1,
                    ));
                    register
                })
                .collect();
            let zero = b.alloc_bit(1, false);
            let one = b.alloc_bit(1, false);
            b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
            b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
            b.emit(SIRInstruction::Store(
                stable(3),
                SIROffset::Static(0),
                1,
                zero,
                vec![],
                vec![],
            ));
            let entry = BlockId(0);
            let nested = [b.new_block(), b.new_block(), b.new_block()];
            let join = b.new_block();
            for (level, &condition) in conditions.iter().enumerate() {
                if level > 0 {
                    b.switch_to_block(nested[level - 1]);
                }
                let then = nested[level];
                b.seal_block(if switches & (1 << level) == 0 {
                    SIRTerminator::Branch {
                        cond: condition,
                        true_block: (then, vec![]),
                        false_block: (join, vec![]),
                    }
                } else {
                    SIRTerminator::Switch {
                        selector: condition,
                        cases: vec![SIRSwitchCase {
                            value: 1u8.into(),
                            target: then,
                        }],
                        default: join,
                    }
                });
            }
            b.switch_to_block(nested[2]);
            b.emit(SIRInstruction::Store(
                stable(3),
                SIROffset::Static(0),
                1,
                one,
                vec![],
                vec![],
            ));
            b.seal_block(SIRTerminator::Jump(join, vec![]));
            b.switch_to_block(join);
            // No input can affect this unconditional constant store.
            b.emit(SIRInstruction::Store(
                stable(4),
                SIROffset::Static(0),
                1,
                one,
                vec![],
                vec![],
            ));
            b.seal_block(SIRTerminator::Return);
            let (blocks, register_map, _) = b.drain();
            let units = [Unit {
                entry_block_id: entry,
                blocks,
                register_map,
            }];
            units[0].verify();
            let slicer = CombSlicer::new(&units, HashMap::default());
            for input in 0..3 {
                assert_eq!(
                    slicer.affected_storage([address(input)]),
                    [address(3)].into_iter().collect(),
                    "switches={switches} input={input}"
                );
                for slice in [
                    slicer.affected_slice([address(input)]),
                    slicer.slice([address(3)]),
                ] {
                    slice[0].verify();
                    assert!(stored(&slice).iter().all(|&target| target == address(3)));
                    assert!(stored(&slice).contains(&address(3)));
                    for &condition in &conditions {
                        assert!(
                            slice[0]
                                .blocks
                                .values()
                                .any(|block| selector(&block.terminator) == Some(condition)),
                            "lost nested selector: switches={switches} input={input} condition={condition:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn affected_storage_follows_control_without_tainting_the_join() {
        let units = [settle()];
        let slicer = CombSlicer::new(&units, HashMap::default());
        assert_eq!(
            slicer.affected_storage([address(0)]),
            [address(2), address(3)].into_iter().collect()
        );
        assert_eq!(
            slicer.affected_storage([address(1)]),
            [address(2), address(3), address(4)].into_iter().collect()
        );
        assert!(slicer.affected_storage([address(9)]).is_empty());
    }

    #[test]
    fn slice_keeps_what_decides_a_store() {
        let units = [settle()];
        let slicer = CombSlicer::new(&units, HashMap::default());
        let slice = slicer.slice([address(2)]);
        assert_eq!(stored(&slice), [address(2)]);
        // The branch on `a` decides whether `x` is stored.
        assert!(matches!(
            slice[0].blocks[&BlockId(0)].terminator,
            SIRTerminator::Branch { .. }
        ));
    }

    #[test]
    fn slice_folds_branches_it_does_not_need() {
        let units = [settle()];
        let slicer = CombSlicer::new(&units, HashMap::default());
        let slice = slicer.slice([address(4)]);
        assert_eq!(stored(&slice), [address(4)]);
        assert_eq!(slice[0].blocks.len(), 2, "the branch on `a` folds away");
        assert!(
            instructions(&slice[0]).all(|inst| inst
                .memory_read()
                .is_none_or(|read| read.addr.absolute_addr() == address(1))),
            "`a` is no longer read"
        );
    }

    /// `for i in 0..a { z = c; }`
    fn looping_settle() -> Unit {
        let mut b = Builder::new();
        let [a, c, z] = [0, 1, 4];
        let bound = b.alloc_bit(8, false);
        b.emit(SIRInstruction::Load(
            bound,
            stable(a),
            SIROffset::Static(0),
            8,
        ));
        let zero = b.alloc_bit(8, false);
        b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        let index = b.alloc_bit(8, false);
        let header = b.new_block_with(vec![index]);
        let body = b.new_block();
        let exit = b.new_block();
        b.seal_block(SIRTerminator::Jump(header, vec![zero]));
        b.switch_to_block(header);
        let more = b.alloc_bit(1, false);
        b.emit(SIRInstruction::Binary(more, index, BinaryOp::LtU, bound));
        b.seal_block(SIRTerminator::Branch {
            cond: more,
            true_block: (body, Vec::new()),
            false_block: (exit, Vec::new()),
        });
        b.switch_to_block(body);
        let value = b.alloc_bit(8, false);
        b.emit(SIRInstruction::Load(
            value,
            stable(c),
            SIROffset::Static(0),
            8,
        ));
        b.emit(SIRInstruction::Store(
            stable(z),
            SIROffset::Static(0),
            8,
            value,
            Vec::new(),
            Vec::new(),
        ));
        let one = b.alloc_bit(8, false);
        b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
        let next = b.alloc_bit(8, false);
        b.emit(SIRInstruction::Binary(next, index, BinaryOp::Add, one));
        b.seal_block(SIRTerminator::Jump(header, vec![next]));
        b.switch_to_block(exit);
        b.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = b.drain();
        Unit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        }
    }

    #[test]
    fn slice_of_storage_no_unit_stores_is_empty() {
        let units = [looping_settle()];
        let slicer = CombSlicer::new(&units, HashMap::default());
        // `a` is an input of the settle: nothing computes it, not even the
        // loop it bounds.
        assert!(slicer.slice([address(0)]).is_empty());
        let slice = slicer.slice([address(0), address(4)]);
        assert_eq!(stored(&slice), [address(4)]);
    }
}
