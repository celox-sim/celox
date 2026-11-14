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

struct StabilityAnalysis {
    partition_flags: Option<AbsoluteAddr>,
}

struct Indexed {
    definitions: HashMap<RegisterId, Definition>,
    /// The predecessors of each block, with the arguments of their edge.
    incoming: HashMap<BlockId, Vec<(BlockId, Vec<RegisterId>)>>,
    /// Immediate controlling branch/switch blocks and their selectors.
    control: HashMap<BlockId, Vec<(BlockId, RegisterId)>>,
    #[expect(
        clippy::disallowed_types,
        reason = "single-threaded: a lazily computed index of completed-only control dependencies, filled behind &self by the slicer that owns it"
    )]
    completed_control: std::cell::OnceCell<HashMap<BlockId, Vec<(BlockId, RegisterId)>>>,
}

/// Selected instruction positions and pruned control flow. Keep the original
/// instructions and register types borrowed until an executable slice is needed.
struct Selection<'a> {
    source: &'a Unit,
    blocks: HashMap<BlockId, SelectedBlock>,
}

struct SelectedBlock {
    params: Vec<RegisterId>,
    instructions: Vec<usize>,
    terminator: SIRTerminator,
}

impl Selection<'_> {
    fn loads(&self) -> impl Iterator<Item = AbsoluteAddr> + '_ {
        self.blocks.iter().flat_map(|(id, block)| {
            block.instructions.iter().flat_map(|&position| {
                loaded_addresses(&self.source.blocks[id].instructions[position])
            })
        })
    }

    fn materialize(self) -> Unit {
        Unit {
            entry_block_id: self.source.entry_block_id,
            register_map: self.source.register_map.clone(),
            blocks: self
                .blocks
                .into_iter()
                .map(|(id, block)| {
                    let source = &self.source.blocks[&id];
                    (
                        id,
                        celox_sir::BasicBlock {
                            id: source.id,
                            params: block.params,
                            instructions: block
                                .instructions
                                .into_iter()
                                .map(|position| source.instructions[position].clone())
                                .collect(),
                            terminator: block.terminator,
                        },
                    )
                })
                .collect(),
        }
    }
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
fn post_dominators(unit: &Unit, completed_only: bool) -> HashMap<BlockId, Option<BlockId>> {
    let mut predecessors: HashMap<BlockId, Vec<BlockId>> = HashMap::default();
    let mut exits = Vec::new();
    for (&id, block) in &unit.blocks {
        let next = successors(&block.terminator);
        if next.is_empty() && (!completed_only || matches!(block.terminator, SIRTerminator::Return))
        {
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

#[expect(
    clippy::disallowed_types,
    reason = "single-threaded: creates the empty lazily filled completed-control index of a slicer"
)]
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
    Indexed {
        definitions,
        incoming,
        control: control_dependencies(unit, false),
        completed_control: std::cell::OnceCell::new(),
    }
}

fn control_dependencies(
    unit: &Unit,
    completed_only: bool,
) -> HashMap<BlockId, Vec<(BlockId, RegisterId)>> {
    // A block is control dependent on a branch when it lies on a path from
    // one of the branch's successors up to (not including) the branch's
    // post-dominator.
    let ipdom = post_dominators(unit, completed_only);
    let mut control: HashMap<BlockId, Vec<(BlockId, RegisterId)>> = HashMap::default();
    for (&id, block) in &unit.blocks {
        let Some(selector) = selector(&block.terminator) else {
            continue;
        };
        // A block that cannot reach an exit has no post-dominator: treat
        // everything it reaches as depending on it.
        let stop = ipdom.get(&id).copied().flatten();
        for (target, _) in successors(&block.terminator) {
            if completed_only && !ipdom.contains_key(&target) {
                continue;
            }
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
    control
}

impl<'a> CombSlicer<'a> {
    pub(crate) fn new(units: &'a [Unit], storage: HashMap<AbsoluteAddr, AbsoluteAddr>) -> Self {
        let mut stores: HashMap<AbsoluteAddr, Vec<(usize, BlockId, usize)>> = HashMap::default();
        let representative =
            |address: AbsoluteAddr| storage.get(&address).copied().unwrap_or(address);
        for (unit_index, unit) in units.iter().enumerate() {
            for (&id, block) in &unit.blocks {
                for (position, inst) in block.instructions.iter().enumerate() {
                    for write in written_storage(inst) {
                        stores
                            .entry(representative(write.absolute_addr()))
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
                    // A call's every write depends on its every read.
                    let storage = written_storage(instruction)
                        .map(|write| Need::Storage(self.representative(write.absolute_addr())));
                    for target in register.into_iter().chain(storage) {
                        edge(block_need, target);
                        instruction
                            .for_each_use(|register| edge(Need::Register(unit, register), target));
                        for read in read_storage(instruction) {
                            edge(
                                Need::Storage(self.representative(read.absolute_addr())),
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
            .into_iter()
            .map(Selection::materialize)
            .collect()
    }

    /// The part of the settle that computes `reads`: every store of storage
    /// they or the part load, and what decides it. Stores the part does not
    /// need, and the settle's other effects, are left to the settle.
    pub(crate) fn slice(&self, reads: impl IntoIterator<Item = AbsoluteAddr>) -> Vec<Unit> {
        self.slice_with_control(reads, false)
    }

    /// Storage in the data/control cone, and its external inputs, when that
    /// cone retains no state. This is an analysis of a completed settle, not
    /// an executable slice: unrelated errors and loops must still execute in
    /// the full settle but do not make the sink's value depend on their inputs.
    pub(crate) fn stateless_cone(
        &self,
        signal: AbsoluteAddr,
        partition_flags: Option<AbsoluteAddr>,
    ) -> Option<(HashSet<AbsoluteAddr>, HashSet<AbsoluteAddr>)> {
        let signal = self.representative(signal);
        if !self.stores.contains_key(&signal) {
            return None;
        }
        let mut kept = vec![HashSet::default(); self.units.len()];
        let mut seen = HashSet::default();
        self.walk(
            &mut vec![Need::Storage(signal)],
            &mut seen,
            &mut kept,
            None,
            Some(&StabilityAnalysis { partition_flags }),
        );
        let units: Vec<_> = self
            .build(&kept, &seen, partition_flags)
            .into_iter()
            .map(Selection::materialize)
            .collect();
        let storage = |address| self.representative(address);
        let writes: HashSet<_> = units
            .iter()
            .flat_map(instructions)
            .flat_map(written_storage)
            .map(|target| storage(target.absolute_addr()))
            .collect();
        let retained = retained_storage(&units, &storage, &writes);
        if !retained.is_empty() {
            tracing::debug!(target: "celox::scheduler", ?signal, instructions = units.iter().flat_map(instructions).count(), retained = ?retained.iter().take(8).collect::<Vec<_>>(), "event cone retains state");
            tracing::trace!(target: "celox::scheduler", ?signal, instructions = ?units.iter().flat_map(instructions).take(64).collect::<Vec<_>>(), "event cone instructions");
            return None;
        }
        let inputs = units
            .iter()
            .flat_map(loads)
            .map(storage)
            .filter(|input| !writes.contains(input))
            .collect();
        Some((writes, inputs))
    }

    /// The same loads as `slice(reads)`, including operands of control flow
    /// that cannot fold away, without cloning executable IR to inspect it.
    pub(crate) fn input_signals(
        &self,
        reads: impl IntoIterator<Item = AbsoluteAddr>,
    ) -> HashSet<AbsoluteAddr> {
        self.slice_selected(reads, false, None)
            .iter()
            .flat_map(Selection::loads)
            .collect()
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
            .into_iter()
            .map(Selection::materialize)
            .collect()
    }

    fn slice_selected(
        &self,
        reads: impl IntoIterator<Item = AbsoluteAddr>,
        preserve_control: bool,
        allowed: Option<&HashSet<AbsoluteAddr>>,
    ) -> Vec<Selection<'_>> {
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
        let mut units;
        loop {
            self.walk(&mut work, &mut seen, &mut kept, allowed, None);
            units = self.build(&kept, &seen, None);
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
        stability: Option<&StabilityAnalysis>,
    ) {
        while let Some(need) = work.pop() {
            if !seen.insert(need) {
                continue;
            }
            match need {
                Need::Storage(address) => {
                    if allowed.is_some_and(|allowed| !allowed.contains(&address))
                        || stability
                            .is_some_and(|analysis| analysis.partition_flags == Some(address))
                    {
                        continue;
                    }
                    for &(unit, block, position) in self.stores.get(&address).into_iter().flatten()
                    {
                        kept[unit].insert((block, position));
                        let inst = &self.units[unit].blocks[&block].instructions[position];
                        self.keep(unit, inst, work);
                        work.push(Need::Block(unit, block));
                    }
                }
                Need::Register(unit, register) => {
                    match self.indexed[unit].definitions.get(&register) {
                        Some(&Definition::Instruction(block, position)) => {
                            kept[unit].insert((block, position));
                            let inst = &self.units[unit].blocks[&block].instructions[position];
                            self.keep(unit, inst, work);
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
                    let control = if stability.is_some() {
                        self.indexed[unit]
                            .completed_control
                            .get_or_init(|| control_dependencies(&self.units[unit], true))
                    } else {
                        &self.indexed[unit].control
                    };
                    for &(controller, selector) in control.get(&block).into_iter().flatten() {
                        work.push(Need::Register(unit, selector));
                        work.push(Need::Block(unit, controller));
                    }
                }
            }
        }
    }

    /// What a kept instruction of `unit` needs: its operands and the
    /// storage it reads. A kept call writes all it writes, in a slice as in
    /// the settle, so the slice needs the rest of the settle's writes of that
    /// storage too: they and the call leave it as the settle does, where the
    /// call alone could leave a value the settle overwrites.
    fn keep(&self, unit: usize, inst: &SIRInstruction<RegionedAbsoluteAddr>, work: &mut Vec<Need>) {
        inst.for_each_use(|register| work.push(Need::Register(unit, register)));
        for read in read_storage(inst) {
            work.push(Need::Storage(self.representative(read.absolute_addr())));
        }
        if inst.call_effects().is_some() {
            for write in written_storage(inst) {
                work.push(Need::Storage(self.representative(write.absolute_addr())));
            }
        }
    }

    fn build(
        &self,
        kept: &[HashSet<(BlockId, usize)>],
        seen: &HashSet<Need>,
        partition_flags: Option<AbsoluteAddr>,
    ) -> Vec<Selection<'_>> {
        self.units
            .iter()
            .enumerate()
            .map(|(unit_index, unit)| {
                // Visit only selected instructions, rather than scanning the
                // whole settle for every cone and every control-flow iteration.
                let mut positions: HashMap<BlockId, Vec<usize>> = HashMap::default();
                for &(block, position) in &kept[unit_index] {
                    positions.entry(block).or_default().push(position);
                }
                for positions in positions.values_mut() {
                    positions.sort_unstable();
                }
                let mut unit = Selection {
                    source: unit,
                    blocks: unit
                        .blocks
                        .iter()
                        .map(|(&id, block)| {
                            (
                                id,
                                SelectedBlock {
                                    params: block.params.clone(),
                                    instructions: positions.remove(&id).unwrap_or_default(),
                                    terminator: block.terminator.clone(),
                                },
                            )
                        })
                        .collect(),
                };
                let needed =
                    |register: &RegisterId| seen.contains(&Need::Register(unit_index, *register));
                // Unneeded parameters go with their arguments.
                let dropped: HashMap<BlockId, Vec<bool>> = unit
                    .blocks
                    .iter()
                    .map(|(&id, block)| (id, block.params.iter().map(|p| !needed(p)).collect()))
                    .collect();
                for (&id, block) in unit.blocks.iter_mut() {
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
                if let Some(flags) = partition_flags {
                    let mut known = HashMap::default();
                    for block in unit.blocks.values_mut() {
                        if let SIRTerminator::Branch {
                            cond,
                            true_block,
                            false_block,
                        } = &block.terminator
                            && let Some((value, true)) =
                                full_partition_value(*cond, flags, self, unit_index, &mut known)
                        {
                            let (target, arguments) =
                                if value != 0 { true_block } else { false_block };
                            block.terminator = SIRTerminator::Jump(*target, arguments.clone());
                        }
                    }
                }
                fold_empty_branches(&mut unit);
                unit
            })
            .collect()
    }
}

/// Evaluate compiler-owned partition guards as in a full settle. Every
/// flag byte is one. This analysis-only view makes cached intermediate reads
/// follow their producers, without treating the cache as a semantic latch.
fn full_partition_value(
    register: RegisterId,
    flags: AbsoluteAddr,
    slicer: &CombSlicer<'_>,
    unit: usize,
    known: &mut HashMap<RegisterId, Option<(u64, bool)>>,
) -> Option<(u64, bool)> {
    if let Some(value) = known.get(&register) {
        return *value;
    }
    known.insert(register, None);
    let result = (|| {
        let &Definition::Instruction(block, position) =
            slicer.indexed[unit].definitions.get(&register)?
        else {
            return None;
        };
        match &slicer.units[unit].blocks[&block].instructions[position] {
            SIRInstruction::Load(_, source, offset, width)
                if source.region == STABLE_REGION
                    && slicer.representative(source.absolute_addr()) == flags
                    && *width <= 64 =>
            {
                let start = offset.constant_bit_offset()?;
                let value = (0..*width)
                    .filter(|bit| (start + bit).is_multiple_of(8))
                    .fold(0u64, |value, bit| value | (1u64 << bit));
                Some((value, true))
            }
            SIRInstruction::Imm(_, value) if value.mask == num_bigint::BigUint::default() => {
                Some((u64::try_from(&value.payload).ok()?, false))
            }
            SIRInstruction::Binary(_, left, op, right) => {
                let (left, lf) = full_partition_value(*left, flags, slicer, unit, known)?;
                let (right, rf) = full_partition_value(*right, flags, slicer, unit, known)?;
                let value = match op {
                    BinaryOp::Or => left | right,
                    BinaryOp::And => left & right,
                    BinaryOp::Xor => left ^ right,
                    BinaryOp::Eq | BinaryOp::EqCase => u64::from(left == right),
                    BinaryOp::Ne | BinaryOp::NeCase => u64::from(left != right),
                    _ => return None,
                };
                Some((value, lf || rf))
            }
            SIRInstruction::Mux(_, condition, yes, no) => {
                let (condition, cf) = full_partition_value(*condition, flags, slicer, unit, known)?;
                let selected = if condition != 0 { yes } else { no };
                let (value, vf) = full_partition_value(*selected, flags, slicer, unit, known)?;
                Some((value, cf || vf))
            }
            _ => None,
        }
    })();
    known.insert(register, result);
    result
}

/// Replace branches whose arms run nothing on the way to one block (with
/// the same arguments) by a jump there, and drop unreachable blocks.
fn fold_empty_branches(unit: &mut Selection<'_>) {
    // Where an empty, parameterless block chain ends, with its arguments.
    let forward = |unit: &Selection<'_>, mut block: BlockId| -> (BlockId, Vec<RegisterId>) {
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
    let mut work = vec![unit.source.entry_block_id];
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
mod tests;
