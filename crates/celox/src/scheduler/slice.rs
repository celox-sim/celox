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
    /// The branch and switch selectors each block is control dependent on.
    control: HashMap<BlockId, Vec<RegisterId>>,
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
    let mut control: HashMap<BlockId, Vec<RegisterId>> = HashMap::default();
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
                control.entry(current).or_default().push(selector);
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

    /// The part of the settle that computes `reads`: every store of storage
    /// they or the part load, and what decides it. Stores the part does not
    /// need, and the settle's other effects, are left to the settle.
    pub(crate) fn slice(&self, reads: impl IntoIterator<Item = AbsoluteAddr>) -> Vec<Unit> {
        let mut kept: Vec<HashSet<(BlockId, usize)>> = vec![HashSet::default(); self.units.len()];
        let mut seen: HashSet<Need> = HashSet::default();
        let mut work: Vec<Need> = reads
            .into_iter()
            .map(|address| Need::Storage(self.representative(address)))
            .collect();
        let mut units: Vec<Unit>;
        loop {
            self.walk(&mut work, &mut seen, &mut kept);
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
    ) {
        while let Some(need) = work.pop() {
            if !seen.insert(need) {
                continue;
            }
            match need {
                Need::Storage(address) => {
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
                    for &selector in self.indexed[unit].control.get(&block).into_iter().flatten() {
                        work.push(Need::Register(unit, selector));
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
}
