//! Drop the marks of partitions that no longer touch the state they were
//! planned to read.
//!
//! The plan lists a partition as a reader of every source its paths load,
//! whole-variable copies included. When the layout aliases such a copy to
//! its source, the copy's store is removed and the partition is left with no
//! access to the source, yet every writer of the source still marks it: a
//! clock copied into an instance port drags the unrelated logic sharing its
//! partition along on every toggle. After the alias stores are gone, each
//! partition's body says what it reads and writes; a mark of a partition
//! whose body touches nothing of the written object is dropped, both in the
//! units outside the settle ([`SettleLayout::objects`]) and in the settle's
//! own publish blocks.
//!
//! Everything a body uses counts, wherever it is computed: a load the
//! optimizer hoisted out of the body still feeds it. A partition whose body
//! cannot be delimited, or that reaches state through a call, keeps every
//! mark.
use crate::AbsoluteAddr;
use crate::ir::{RegionedAbsoluteAddr, RegisterId, RuntimeProgram, SIRInstruction, SirProgram};
use celox_design::{BinaryOp, STABLE_REGION};
use celox_sir::effects::StateAccess;
use celox_sir::{BlockId, ExecutionUnit, SIROffset, SIRTerminator, SIRValue};

type Unit = ExecutionUnit<RegionedAbsoluteAddr>;
type Instruction = SIRInstruction<RegionedAbsoluteAddr>;
/// The stable-region homes (layout offsets) a partition touches.
type Homes = crate::HashSet<usize>;

/// Prune the stale marks of the partitioned settle of `sir` (see the module
/// documentation). Runs after the alias stores are removed, and after any
/// dead-store elimination, before the units outside the settle are
/// instrumented.
pub(crate) fn prune_stale_marks(
    sir: &mut SirProgram,
    layout: &mut crate::backend::MemoryLayout,
    runtime: &RuntimeProgram,
) {
    let Some(schema) = runtime.runtime_schema.settle_partitions.as_ref() else {
        return;
    };
    let Some(settle) = layout.settle.as_mut() else {
        return;
    };
    // Lane-partitioned programs carry their own copies of the settle.
    if sir.parallel.is_some() {
        return;
    }
    let flags = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, schema.flags);
    let partitions = settle.partitions;
    // What each partition touches and writes, once every body found for it
    // resolved; a partition with an unresolved body keeps every mark.
    let mut touched: Vec<Option<Homes>> = vec![None; partitions];
    let mut written: Vec<Option<Homes>> = vec![None; partitions];
    let mut unresolved = vec![false; partitions];
    let mut bodies: Vec<Vec<(usize, Vec<BlockId>)>> = Vec::new();
    // An alias store leaves its load behind: only live values count.
    for unit in &mut sir.eval_comb {
        celox_sir_opt::optimizer::remove_dead_sir_definitions(unit);
    }
    for unit in &sir.eval_comb {
        let analysis = Analysis::new(unit, &flags, partitions, &layout.offsets);
        let mut unit_bodies = Vec::new();
        for &block in unit.blocks.keys() {
            let Some((partition, body, merge)) = analysis.guard(block) else {
                continue;
            };
            let blocks = analysis.body(body, merge);
            match analysis.effects(&blocks) {
                Some((t, w)) if !unresolved[partition] => {
                    touched[partition].get_or_insert_default().extend(t);
                    written[partition].get_or_insert_default().extend(w);
                }
                _ => {
                    unresolved[partition] = true;
                    touched[partition] = None;
                    written[partition] = None;
                }
            }
            unit_bodies.push((partition, blocks));
        }
        bodies.push(unit_bodies);
    }
    let resolved = touched.iter().filter(|homes| homes.is_some()).count();
    if resolved == 0 {
        return;
    }
    // A whole-variable copy does not publish its target (the plan counts a
    // reader of the target as a reader of the source): a body touching the
    // target depends on the source, and on the source's own sources.
    let mut copy_sources: crate::HashMap<usize, Vec<usize>> = crate::HashMap::default();
    for (target, source) in &schema.copies {
        if let (Some(&target), Some(&source)) =
            (layout.offsets.get(target), layout.offsets.get(source))
            && target != source
        {
            copy_sources.entry(target).or_default().push(source);
        }
    }
    for homes in touched.iter_mut().flatten() {
        let mut work: Vec<usize> = homes.iter().copied().collect();
        while let Some(home) = work.pop() {
            for &source in copy_sources.get(&home).into_iter().flatten() {
                if homes.insert(source) {
                    work.push(source);
                }
            }
        }
    }
    // A reader keeps the mark of a home its body touches; a partition the
    // bodies did not resolve keeps every mark, as do the wait flags.
    let keeps = |reader: usize, homes: &Homes| -> bool {
        reader >= partitions
            || touched[reader]
                .as_ref()
                .is_none_or(|read| read.iter().any(|home| homes.contains(home)))
    };
    let mut dropped_marks = 0usize;
    for object in &mut settle.objects {
        let homes = Homes::from_iter([object.start]);
        let before = object.partitions.len();
        object
            .partitions
            .retain(|&reader| keeps(reader as usize, &homes));
        dropped_marks += before - object.partitions.len();
    }
    let mut stats = PublishStats::default();
    for (unit, unit_bodies) in sir.eval_comb.iter_mut().zip(bodies) {
        let edits = {
            let analysis = Analysis::new(unit, &flags, partitions, &layout.offsets);
            let mut edits = Vec::new();
            let mut seen = crate::HashSet::default();
            for (partition, blocks) in &unit_bodies {
                let publisher_writes = written[*partition].as_ref();
                analysis.publish_edits(
                    blocks,
                    &keeps,
                    publisher_writes,
                    &touched,
                    &mut seen,
                    &mut edits,
                );
            }
            edits
        };
        if !edits.is_empty() {
            stats.add(&apply_edits(unit, &flags, edits));
            celox_sir_opt::optimizer::remove_dead_sir_definitions(unit);
        }
    }
    tracing::debug!(
        "[settle-prune] partitions resolved {resolved}/{partitions}, object marks dropped \
         {dropped_marks}, publish stores dropped {}, narrowed {}, publish branches bypassed {}",
        stats.dropped,
        stats.narrowed,
        stats.bypassed
    );
}

/// What a home lookup found.
enum Home {
    /// A stable object at this layout offset.
    Stable(usize),
    /// State the settle's marks do not cover (another region).
    Elsewhere,
    /// A stable object without a layout offset.
    Unknown,
}

pub(super) struct Analysis<'a> {
    pub(super) unit: &'a Unit,
    pub(super) flags: &'a RegionedAbsoluteAddr,
    pub(super) partitions: usize,
    offsets: &'a crate::HashMap<AbsoluteAddr, usize>,
    pub(super) defs: crate::HashMap<RegisterId, &'a Instruction>,
    /// The block declaring each parameter, and its position.
    params: crate::HashMap<RegisterId, (BlockId, usize)>,
    /// The argument lists passed to each block.
    args: crate::HashMap<BlockId, Vec<&'a [RegisterId]>>,
    /// How many instructions and terminators use each register.
    uses: crate::HashMap<RegisterId, usize>,
}

impl<'a> Analysis<'a> {
    pub(super) fn new(
        unit: &'a Unit,
        flags: &'a RegionedAbsoluteAddr,
        partitions: usize,
        offsets: &'a crate::HashMap<AbsoluteAddr, usize>,
    ) -> Self {
        let mut defs = crate::HashMap::default();
        let mut params = crate::HashMap::default();
        let mut args: crate::HashMap<BlockId, Vec<&'a [RegisterId]>> = crate::HashMap::default();
        let mut uses: crate::HashMap<RegisterId, usize> = crate::HashMap::default();
        for block in unit.blocks.values() {
            for (index, &param) in block.params.iter().enumerate() {
                params.insert(param, (block.id, index));
            }
            for instruction in &block.instructions {
                if let Some(defined) = instruction.defined_register() {
                    defs.insert(defined, instruction);
                }
                instruction.for_each_use(|register| *uses.entry(register).or_default() += 1);
            }
            block
                .terminator
                .for_each_use(|register| *uses.entry(register).or_default() += 1);
            match &block.terminator {
                SIRTerminator::Jump(target, arguments) => {
                    args.entry(*target).or_default().push(arguments);
                }
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => {
                    args.entry(true_block.0).or_default().push(&true_block.1);
                    args.entry(false_block.0).or_default().push(&false_block.1);
                }
                SIRTerminator::Switch { .. } | SIRTerminator::Return | SIRTerminator::Error(_) => {}
            }
        }
        Self {
            unit,
            flags,
            partitions,
            offsets,
            defs,
            params,
            args,
            uses,
        }
    }

    /// The stores of `block` that store a load of the same bits of the same
    /// home back, with nothing stored to the home in between: copies between
    /// two aliases of one object, which a later pass removes and which change
    /// nothing meanwhile. Returned as the source register of each.
    fn identity_stores(
        &self,
        block: &celox_sir::BasicBlock<RegionedAbsoluteAddr>,
    ) -> Vec<RegisterId> {
        // Loads in this block: register -> (home, offset, width, position).
        let mut loads: crate::HashMap<RegisterId, (usize, &SIROffset, usize, usize)> =
            crate::HashMap::default();
        let mut last_store: crate::HashMap<usize, usize> = crate::HashMap::default();
        let mut identity = Vec::new();
        for (position, instruction) in block.instructions.iter().enumerate() {
            match instruction {
                SIRInstruction::Load(defined, address, offset, width) => {
                    if let Home::Stable(home) = self.home(address) {
                        loads.insert(*defined, (home, offset, *width, position));
                    }
                }
                SIRInstruction::Store(target, offset, width, source, triggers, sites) => {
                    let Home::Stable(home) = self.home(target) else {
                        continue;
                    };
                    if triggers.is_empty()
                        && sites.is_empty()
                        && let Some(&(loaded, loaded_offset, loaded_width, loaded_at)) =
                            loads.get(source)
                        && loaded == home
                        && loaded_offset == offset
                        && loaded_width == *width
                        && last_store.get(&home).is_none_or(|&at| at < loaded_at)
                    {
                        identity.push(*source);
                    } else {
                        last_store.insert(home, position);
                    }
                }
                _ => {}
            }
        }
        identity
    }

    fn home(&self, address: &RegionedAbsoluteAddr) -> Home {
        if address.region != STABLE_REGION {
            return Home::Elsewhere;
        }
        match self.offsets.get(&address.absolute_addr()) {
            Some(&start) => Home::Stable(start),
            None => Home::Unknown,
        }
    }

    /// The partition a load of `register` guards: a byte of the flags below
    /// the untracked byte.
    pub(super) fn flag_load(&self, register: RegisterId) -> Option<usize> {
        match self.defs.get(&register)? {
            SIRInstruction::Load(_, address, SIROffset::Static(offset), 8)
                if address == self.flags
                    && offset.is_multiple_of(8)
                    && offset / 8 < self.partitions =>
            {
                Some(offset / 8)
            }
            _ => None,
        }
    }

    pub(super) fn is_zero(&self, register: RegisterId) -> bool {
        matches!(
            self.defs.get(&register),
            Some(SIRInstruction::Imm(_, value)) if *value == SIRValue::new(0u32)
        )
    }

    /// The partition `block` guards (`Load own flag; Ne 0; Branch`), with
    /// the first block of its body and the block that skips it.
    pub(super) fn guard(&self, block: BlockId) -> Option<(usize, BlockId, BlockId)> {
        let SIRTerminator::Branch {
            cond,
            true_block: (body, body_args),
            false_block: (merge, _),
        } = &self.unit.blocks.get(&block)?.terminator
        else {
            return None;
        };
        if !body_args.is_empty() {
            return None;
        }
        let SIRInstruction::Binary(_, lhs, BinaryOp::Ne, rhs) = self.defs.get(cond)? else {
            return None;
        };
        let partition = match (self.flag_load(*lhs), self.flag_load(*rhs)) {
            (Some(partition), None) if self.is_zero(*rhs) => partition,
            (None, Some(partition)) if self.is_zero(*lhs) => partition,
            _ => return None,
        };
        Some((partition, *body, *merge))
    }

    /// The blocks reachable from `body` without passing `merge`.
    pub(super) fn body(&self, body: BlockId, merge: BlockId) -> Vec<BlockId> {
        let mut seen = crate::HashSet::from_iter([body]);
        let mut stack = vec![body];
        let mut blocks = Vec::new();
        while let Some(block) = stack.pop() {
            blocks.push(block);
            let Some(block) = self.unit.blocks.get(&block) else {
                continue;
            };
            let mut next = |target: BlockId| {
                if target != merge && seen.insert(target) {
                    stack.push(target);
                }
            };
            match &block.terminator {
                SIRTerminator::Jump(target, _) => next(*target),
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => {
                    next(true_block.0);
                    next(false_block.0);
                }
                SIRTerminator::Switch { cases, default, .. } => {
                    for case in cases {
                        next(case.target);
                    }
                    next(*default);
                }
                SIRTerminator::Return | SIRTerminator::Error(_) => {}
            }
        }
        blocks
    }

    /// Add the homes `access` may touch; `None` when it may touch anything.
    fn add_access(
        &self,
        homes: &mut Homes,
        access: StateAccess<'_, RegionedAbsoluteAddr>,
    ) -> Option<()> {
        let mut add = |address: &RegionedAbsoluteAddr| match self.home(address) {
            Home::Stable(start) => {
                homes.insert(start);
                Some(())
            }
            Home::Elsewhere => Some(()),
            Home::Unknown => None,
        };
        match access {
            StateAccess::None => Some(()),
            StateAccess::Access(access) => add(access.addr),
            StateAccess::Objects(objects) => objects.iter().try_for_each(add),
            StateAccess::Everything => None,
        }
    }

    /// The homes the instructions of `blocks` touch (read or write) and write;
    /// `None` when a body reaches state in a way this cannot follow, or ends the
    /// unit.
    fn effects(&self, blocks: &[BlockId]) -> Option<(Homes, Homes)> {
        let mut touched = Homes::default();
        let mut written = Homes::default();
        let mut work: Vec<RegisterId> = Vec::new();
        // The loads that only feed identity stores, with those stores.
        let mut identity_loads: crate::HashMap<RegisterId, usize> = crate::HashMap::default();
        for id in blocks {
            let block = self.unit.blocks.get(id)?;
            for source in self.identity_stores(block) {
                *identity_loads.entry(source).or_default() += 1;
            }
        }
        identity_loads.retain(|register, count| self.uses.get(register) == Some(count));
        for id in blocks {
            let block = self.unit.blocks.get(id)?;
            for instruction in &block.instructions {
                if let SIRInstruction::Store(_, _, _, source, ..) = instruction
                    && identity_loads.contains_key(source)
                {
                    continue;
                }
                if let Some(defined) = instruction.defined_register()
                    && identity_loads.contains_key(&defined)
                {
                    continue;
                }
                self.add_access(&mut touched, instruction.state_reads())?;
                self.add_access(&mut touched, instruction.state_writes())?;
                self.add_access(&mut written, instruction.state_writes())?;
                instruction.for_each_use(|register| work.push(register));
            }
            if matches!(block.terminator, SIRTerminator::Return) {
                return None;
            }
            block
                .terminator
                .for_each_use(|register| work.push(register));
        }
        let mut seen = crate::HashSet::default();
        while let Some(register) = work.pop() {
            if !seen.insert(register) {
                continue;
            }
            if let Some(instruction) = self.defs.get(&register) {
                self.add_access(&mut touched, instruction.state_reads())?;
                instruction.for_each_use(|register| work.push(register));
            } else if let Some(&(block, index)) = self.params.get(&register) {
                let arguments = self.args.get(&block)?;
                for arguments in arguments {
                    work.push(*arguments.get(index)?);
                }
            } else {
                return None;
            }
        }
        Some((touched, written))
    }

    /// The homes the instructions of `blocks` touch, through the values they
    /// use wherever those are computed; `None` when a body reaches state in
    /// a way this cannot follow, or ends the unit.
    #[cfg(test)]
    fn touched(&self, blocks: &[BlockId]) -> Option<Homes> {
        self.effects(blocks).map(|(touched, _)| touched)
    }

    /// The homes a change test compares: an `Or` tree of `NeCase` leaves,
    /// each comparing a load of the object with its new value.
    fn compared(&self, cond: RegisterId, homes: &mut Homes) -> Option<()> {
        match self.defs.get(&cond)? {
            SIRInstruction::Binary(_, lhs, BinaryOp::Or, rhs) => {
                self.compared(*lhs, homes)?;
                self.compared(*rhs, homes)
            }
            SIRInstruction::Binary(_, lhs, BinaryOp::NeCase, rhs) => {
                let mut found = false;
                for operand in [lhs, rhs] {
                    if let Some(SIRInstruction::Load(_, address, ..)) = self.defs.get(operand)
                        && let Home::Stable(start) = self.home(address)
                    {
                        homes.insert(start);
                        found = true;
                    }
                }
                found.then_some(())
            }
            _ => None,
        }
    }

    /// The flag stores of a publish block (`Store(flags, ..)` of byte 1, or
    /// of one or more words ORed with byte masks), as `(instruction index,
    /// readers)`; `None` unless the block only publishes.
    pub(super) fn publish_stores(
        &self,
        block: BlockId,
        done: BlockId,
    ) -> Option<Vec<(usize, Vec<usize>)>> {
        let block = self.unit.blocks.get(&block)?;
        if !block.params.is_empty()
            || !matches!(&block.terminator, SIRTerminator::Jump(target, args) if *target == done && args.is_empty())
        {
            return None;
        }
        let mut stores = Vec::new();
        for (index, instruction) in block.instructions.iter().enumerate() {
            match instruction {
                SIRInstruction::Imm(..)
                | SIRInstruction::Binary(_, _, BinaryOp::Or, _)
                | SIRInstruction::Concat(..) => {}
                SIRInstruction::Load(_, address, ..) if address == self.flags => {}
                SIRInstruction::Store(
                    address,
                    SIROffset::Static(offset),
                    width,
                    source,
                    triggers,
                    sites,
                ) if address == self.flags
                    && triggers.is_empty()
                    && sites.is_empty()
                    && offset.is_multiple_of(8)
                    && width.is_multiple_of(8) =>
                {
                    let bytes = if *width == 8 {
                        self.is_one(*source).then_some(vec![true])?
                    } else {
                        self.published_bytes(*source, *width)?
                    };
                    let readers = bytes
                        .iter()
                        .enumerate()
                        .filter(|(_, set)| **set)
                        .map(|(byte, _)| offset / 8 + byte)
                        .collect();
                    stores.push((index, readers));
                }
                _ => return None,
            }
        }
        Some(stores)
    }

    pub(super) fn is_one(&self, register: RegisterId) -> bool {
        matches!(
            self.defs.get(&register),
            Some(SIRInstruction::Imm(_, value)) if *value == SIRValue::new(1u32)
        )
    }

    /// The bytes a word store of `width` bits publishes, lowest first: an
    /// immediate of bytes 0 or 1, alone or ORed into the flags word, or a
    /// concatenation of such words.
    pub(super) fn published_bytes(&self, source: RegisterId, width: usize) -> Option<Vec<bool>> {
        let immediate = |register: RegisterId| -> Option<Vec<bool>> {
            let SIRInstruction::Imm(_, value) = self.defs.get(&register)? else {
                return None;
            };
            if value.mask != 0u32.into() {
                return None;
            }
            let bytes = value.payload.to_bytes_le();
            if bytes.len() > width / 8 || bytes.iter().any(|byte| !matches!(byte, 0 | 1)) {
                return None;
            }
            let mut set: Vec<bool> = bytes.iter().map(|&byte| byte == 1).collect();
            set.resize(width / 8, false);
            Some(set)
        };
        match self.defs.get(&source)? {
            SIRInstruction::Imm(..) => immediate(source),
            SIRInstruction::Binary(_, lhs, BinaryOp::Or, rhs) => {
                let (constant, loaded) = match (immediate(*lhs), immediate(*rhs)) {
                    (Some(bytes), None) => (bytes, *rhs),
                    (None, Some(bytes)) => (bytes, *lhs),
                    _ => return None,
                };
                match self.defs.get(&loaded)? {
                    SIRInstruction::Load(_, address, ..) if address == self.flags => Some(constant),
                    _ => None,
                }
            }
            // Parts from the most significant down: the last is the lowest.
            SIRInstruction::Concat(_, parts) => {
                let mut bytes = Vec::new();
                for part in parts.iter().rev() {
                    let part_width = self.unit.register_map.get(part)?.width();
                    if !part_width.is_multiple_of(8) {
                        return None;
                    }
                    bytes.extend(self.published_bytes(*part, part_width)?);
                }
                (bytes.len() == width / 8).then_some(bytes)
            }
            _ => None,
        }
    }

    /// The edits that drop the stale readers from the publish blocks of a
    /// partition body.
    fn publish_edits(
        &self,
        blocks: &[BlockId],
        keeps: &dyn Fn(usize, &Homes) -> bool,
        publisher_writes: Option<&Homes>,
        touched: &[Option<Homes>],
        seen: &mut crate::HashSet<BlockId>,
        edits: &mut Vec<Edit>,
    ) {
        for &block in blocks {
            let Some(SIRTerminator::Branch {
                cond,
                true_block: (publish, publish_args),
                false_block: (done, done_args),
            }) = self.unit.blocks.get(&block).map(|block| &block.terminator)
            else {
                continue;
            };
            // A body that could not be delimited may hold another body's
            // publish blocks: edit each once.
            if !publish_args.is_empty() || !blocks.contains(publish) || !seen.insert(*publish) {
                continue;
            }
            let mut compared = Homes::default();
            if self.compared(*cond, &mut compared).is_none() {
                continue;
            }
            let Some(stores) = self.publish_stores(*publish, *done) else {
                continue;
            };
            let mut remaining = 0usize;
            let mut changes = Vec::new();
            for (index, readers) in stores {
                let kept = readers
                    .iter()
                    .copied()
                    .filter(|&reader| {
                        // A reader that touches any home that the publisher partition
                        // writes depends on the publisher's work: keep it even if
                        // this specific change test compared another object.
                        if let (Some(pub_writes), Some(reader_homes)) = (
                            publisher_writes,
                            touched.get(reader).and_then(|h| h.as_ref()),
                        ) {
                            if reader_homes.iter().any(|home| pub_writes.contains(home)) {
                                return true;
                            }
                        }
                        keeps(reader, &compared)
                    })
                    .collect::<Vec<_>>();
                remaining += kept.len();
                if kept.len() != readers.len() {
                    changes.push((index, kept));
                }
            }
            if changes.is_empty() {
                continue;
            }
            edits.push(Edit {
                guard: block,
                publish: *publish,
                done: *done,
                done_args: done_args.clone(),
                changes,
                bypass: remaining == 0,
            });
        }
    }
}

/// The changes to one publish block.
struct Edit {
    /// The block whose change test branches to the publish block.
    guard: BlockId,
    publish: BlockId,
    done: BlockId,
    done_args: Vec<RegisterId>,
    /// The flag stores to replace, by instruction index, with the readers
    /// they keep (none: drop the store).
    changes: Vec<(usize, Vec<usize>)>,
    /// Whether no flag store remains: the test then jumps past the block.
    bypass: bool,
}

#[derive(Default)]
struct PublishStats {
    dropped: usize,
    narrowed: usize,
    bypassed: usize,
}

impl PublishStats {
    fn add(&mut self, other: &Self) {
        self.dropped += other.dropped;
        self.narrowed += other.narrowed;
        self.bypassed += other.bypassed;
    }
}

fn apply_edits(unit: &mut Unit, flags: &RegionedAbsoluteAddr, edits: Vec<Edit>) -> PublishStats {
    let mut stats = PublishStats::default();
    let mut next_register = unit
        .register_map
        .keys()
        .map(|register| register.0 + 1)
        .max()
        .unwrap_or(0);
    for edit in edits {
        let mut one = None;
        let mut replacements: Vec<(usize, Vec<Instruction>)> = Vec::new();
        for (index, kept) in edit.changes {
            if kept.is_empty() {
                stats.dropped += 1;
                replacements.push((index, Vec::new()));
                continue;
            }
            stats.narrowed += 1;
            let one = *one.get_or_insert_with(|| {
                let register = RegisterId(next_register);
                next_register += 1;
                unit.register_map.insert(register, super::byte_type());
                register
            });
            let mut stores = vec![SIRInstruction::Imm(one, SIRValue::new(1u32))];
            stores.extend(kept.into_iter().map(|reader| {
                SIRInstruction::Store(
                    *flags,
                    SIROffset::Static(reader * 8),
                    8,
                    one,
                    Vec::new(),
                    Vec::new(),
                )
            }));
            replacements.push((index, stores));
        }
        let publish = unit
            .blocks
            .get_mut(&edit.publish)
            .expect("the publish block of an edit");
        // The first `one` must precede the stores: rebuild the instruction
        // list once, in order.
        let mut instructions = Vec::with_capacity(publish.instructions.len());
        let mut defined_one = false;
        for (index, instruction) in publish.instructions.drain(..).enumerate() {
            match replacements.iter().position(|(at, _)| *at == index) {
                Some(position) => {
                    let (_, replacement) = replacements.swap_remove(position);
                    for instruction in replacement {
                        if matches!(instruction, SIRInstruction::Imm(..)) {
                            if defined_one {
                                continue;
                            }
                            defined_one = true;
                        }
                        instructions.push(instruction);
                    }
                }
                None => instructions.push(instruction),
            }
        }
        publish.instructions = instructions;
        if edit.bypass {
            stats.bypassed += 1;
            let guard = unit
                .blocks
                .get_mut(&edit.guard)
                .expect("the guard block of an edit");
            guard.terminator = SIRTerminator::Jump(edit.done, edit.done_args);
            // The publish block is unreachable once nothing else jumps to it.
            let referenced = unit.blocks.values().any(|block| {
                let mut found = false;
                visit_targets(&block.terminator, |target| found |= target == edit.publish);
                found
            });
            if !referenced {
                unit.blocks.remove(&edit.publish);
            }
        }
    }
    stats
}

fn visit_targets(terminator: &SIRTerminator, mut visit: impl FnMut(BlockId)) {
    match terminator {
        SIRTerminator::Jump(target, _) => visit(*target),
        SIRTerminator::Branch {
            true_block,
            false_block,
            ..
        } => {
            visit(true_block.0);
            visit(false_block.0);
        }
        SIRTerminator::Switch { cases, default, .. } => {
            for case in cases {
                visit(case.target);
            }
            visit(*default);
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox_sir_opt::SettlePartition;

    /// A settle value copied into an instance port: the plan marks the copy's
    /// partition whenever the value changes, the layout aliases the copy, and
    /// the mark goes with the copy's store, while the port's reader keeps
    /// its own. Every mark that stays names a partition whose body touches
    /// the object.
    #[test]
    fn aliased_copies_leave_their_partitions_unmarked() {
        let source = r#"
module Sub (d: input logic<8>, q: output logic<8>) {
    assign q = d ^ 8'h0f;
}
module Top (
    a: input logic<8>,
    b: input logic<8>,
    c: input logic<8>,
    y: output logic<8>,
    z: output logic<8>,
    q: output logic<8>,
) {
    var m: logic<8>;
    var n: logic<8>;
    var o: logic<8>;
    assign m = a + b;
    assign n = (m ^ c) + 8'd1;
    assign o = (n & a) | (m >> 1);
    assign y = o + n + m;
    assign z = (o ^ 8'h5a) - (n & b) + (m | c);
    inst sub: Sub (d: o, q);
}
"#;
        let options = crate::optimizer::OptimizeOptions::default()
            .enable(crate::optimizer::SirPass::PartitionedSettle)
            .with_settle_partition(SettlePartition::Always);
        let (program, _) = crate::compile_to_sir(
            &[(source, std::path::Path::new("settle-prune.veryl"))],
            "Top",
            &[],
            &[],
            false,
            &Default::default(),
            None,
            None,
            None,
            None,
            &[],
            &options,
        )
        .unwrap();
        let o = program.get_addr(&[], &["o"]).unwrap();
        let sub_d = program.get_addr(&[("sub", 0)], &["d"]).unwrap();
        let schema = program
            .runtime
            .runtime_schema
            .settle_partitions
            .clone()
            .expect("the settle is partitioned");
        let planned = |address| -> Vec<u32> {
            schema
                .marks
                .iter()
                .filter(|(marked, _)| *marked == address)
                .flat_map(|(_, partitions)| partitions.iter().copied())
                .filter(|&partition| partition < schema.partitions)
                .collect()
        };
        // Nothing in the settle reads the port: its only mark is the copy's
        // partition, which the plan also lists as a reader of the source.
        // The port is marked for its copy and its reader, both planned
        // readers of the source.
        let copy = planned(sub_d);
        assert_eq!(
            copy.len(),
            2,
            "the copy's partition and the reader's: {copy:?}"
        );
        assert!(copy.iter().all(|partition| planned(o).contains(partition)));
        let program = program.into_laid_out(false);
        let layout = program.layout();
        let start = layout.offsets[&o];
        assert_eq!(layout.offsets[&sub_d], start, "the port copy is an alias");
        let settle = layout.settle.as_ref().unwrap();
        let object = settle
            .objects
            .iter()
            .find(|object| object.start == start)
            .expect("the source is a settle object");
        let kept: Vec<u32> = copy
            .iter()
            .copied()
            .filter(|partition| object.partitions.contains(partition))
            .collect();
        assert_eq!(
            kept.len(),
            1,
            "the reader's partition stays marked, the copy's does not: {:?} of {copy:?}",
            object.partitions
        );
        // Every mark that stays names a partition whose body touches the object.
        let partitions = settle.partitions;
        let flags = RegionedAbsoluteAddr::from_absolute_addr(STABLE_REGION, schema.flags);
        let mut checked = 0;
        for unit in &program.sir.eval_comb {
            let analysis = Analysis::new(unit, &flags, partitions, &layout.offsets);
            for &block in unit.blocks.keys() {
                let Some((partition, body, merge)) = analysis.guard(block) else {
                    continue;
                };
                let touched = analysis.touched(&analysis.body(body, merge)).unwrap();
                for object in &settle.objects {
                    if object.partitions.contains(&(partition as u32)) {
                        checked += 1;
                        assert!(
                            touched.contains(&object.start),
                            "partition {partition} is marked for {}..{} it does not touch",
                            object.start,
                            object.end
                        );
                    }
                }
            }
        }
        assert!(checked > 0);
    }

    #[test]
    fn publisher_retains_readers_touching_its_other_homes() {
        let source = r#"
module Sub (
    d: input logic<8>,
    q: output logic<8>,
) {
    assign q = d;
}

module Top (
    clk: input logic,
    a: input logic<8>,
    x: output logic<8>,
    y: output logic<8>,
) {
    var v1: logic<8>;
    var v2: logic<8>;
    var sub_q: logic<8>;
    // v1 and v2 are produced together by one partition
    assign v1 = a + 8'd1;
    assign v2 = a ^ 8'hff;
    // Reader partition touches v1 directly, and also sub.d (aliased to v2)
    inst sub: Sub (d: v2, q: sub_q);
    assign x = v1 + sub_q;
    assign y = v2;
}
"#;
        let options = crate::optimizer::OptimizeOptions::default()
            .enable(crate::optimizer::SirPass::PartitionedSettle)
            .with_settle_partition(SettlePartition::Always);
        let (program, _) = crate::compile_to_sir(
            &[(source, std::path::Path::new("settle-prune-multi.veryl"))],
            "Top",
            &[],
            &[],
            false,
            &Default::default(),
            None,
            None,
            None,
            None,
            &[],
            &options,
        )
        .unwrap();
        let program = program.into_laid_out(false);
        let layout = program.layout();
        let settle = layout.settle.as_ref().unwrap();
        assert!(settle.partitions > 0);
    }
}
