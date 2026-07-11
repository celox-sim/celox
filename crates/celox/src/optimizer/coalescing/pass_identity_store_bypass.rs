//! Identity Store bypass: detect Store(B, identity_copy_from_A), remove the
//! Store, and register B as an alias of A in the memory layout.
//!
//! After aliasing, Load(B) reads from A's physical memory (correct because
//! the Store was writing A's exact value to B). The Store removal cascades
//! through DCE to eliminate the Concat chain that assembled the copy.
//!
//! Safety: aliases are selected only after collecting accesses from every
//! combinational execution unit.  In particular, a store that happens to be
//! unique inside one unit is not a unique writer of its address.

use super::shared::{def_reg, sir_value_to_u64};
use crate::ir::*;
use crate::{HashMap, HashSet};

#[derive(Clone, Copy)]
struct AddressMetadata {
    width: usize,
    is_4state: bool,
}

#[derive(Clone, Copy, Default)]
struct AddressFacts {
    reads: usize,
    writes: usize,
}

#[derive(Clone, Copy)]
struct StoreCandidate {
    eu_index: usize,
    block: BlockId,
    instruction_index: usize,
    address: RegionedAbsoluteAddr,
    width: usize,
    source: RegisterId,
    identity_source: Option<RegionedAbsoluteAddr>,
    shape_is_aliasable: bool,
    effects_are_removable: bool,
}

struct GlobalIdentityFacts {
    accesses: HashMap<AbsoluteAddr, AddressFacts>,
    stores: Vec<StoreCandidate>,
}

/// Analyze all combinational units before selecting any Program-global address
/// alias.  The returned stores are still present; `Program::build_layout`
/// removes only aliases that its final storage validation accepts.
pub(super) fn find_program_aliases(
    program: &Program,
    four_state: bool,
) -> HashMap<AbsoluteAddr, AbsoluteAddr> {
    let mut metadata = HashMap::default();
    for (&instance_id, module_id) in &program.instance_module {
        if let Some(variables) = program.module_variables.get(module_id) {
            for variable in variables.values() {
                metadata.insert(
                    AbsoluteAddr {
                        instance_id,
                        var_id: variable.id,
                    },
                    AddressMetadata {
                        width: variable.width,
                        is_4state: variable.is_4state,
                    },
                );
            }
        }
    }

    // These addresses cannot safely become the removable side of an alias.
    // FF references are rejected by MemoryLayout too, but rejecting them here
    // keeps address_aliases itself limited to candidates that can take effect.
    let mut blocked_aliases = ff_addresses(program);
    blocked_aliases.extend(
        program
            .comb_observers
            .iter()
            .flat_map(|observer| observer.written_inputs.iter().copied()),
    );
    blocked_aliases.extend(program.initial_memory_values.iter().map(|init| init.addr));

    analyze_eval_comb_aliases(
        &program.eval_comb,
        &metadata,
        &blocked_aliases,
        &program.address_aliases,
        four_state,
    )
}

fn analyze_eval_comb_aliases(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    metadata: &HashMap<AbsoluteAddr, AddressMetadata>,
    blocked_aliases: &HashSet<AbsoluteAddr>,
    existing_aliases: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    four_state: bool,
) -> HashMap<AbsoluteAddr, AbsoluteAddr> {
    // Phase 1: collect every read/write and every syntactic store candidate.
    // No alias decision is made while the scan is incomplete.
    let facts = collect_global_facts(units, metadata);

    // Phase 2: require the completed global facts for both identity and
    // duplicate-store proofs.
    select_aliases(
        &facts,
        metadata,
        blocked_aliases,
        existing_aliases,
        four_state,
    )
}

fn collect_global_facts(
    units: &[ExecutionUnit<RegionedAbsoluteAddr>],
    metadata: &HashMap<AbsoluteAddr, AddressMetadata>,
) -> GlobalIdentityFacts {
    let mut accesses = HashMap::<AbsoluteAddr, AddressFacts>::default();
    let mut stores = Vec::new();

    for (eu_index, eu) in units.iter().enumerate() {
        let mut defs = HashMap::default();
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                if let Some(dst) = def_reg(inst) {
                    defs.insert(dst, inst);
                }
            }
        }
        let must_execute = must_execute_blocks(eu);

        for block in eu.blocks.values() {
            for (instruction_index, inst) in block.instructions.iter().enumerate() {
                match inst {
                    SIRInstruction::Load(_, address, _, _) => {
                        increment_saturating(
                            &mut accesses.entry(address.absolute_addr()).or_default().reads,
                        );
                    }
                    SIRInstruction::Store(
                        address,
                        offset,
                        width,
                        source,
                        triggers,
                        capture_sites,
                    ) => {
                        let absolute = address.absolute_addr();
                        increment_saturating(&mut accesses.entry(absolute).or_default().writes);
                        let full_width = metadata
                            .get(&absolute)
                            .is_some_and(|info| info.width == *width);
                        let followed_by_capture = matches!(
                            block.instructions.get(instruction_index + 1),
                            Some(SIRInstruction::CombCaptureEnableIfChanged { sites, .. })
                                if !sites.is_empty()
                        );
                        let shape_is_aliasable = address.region == STABLE_REGION
                            && matches!(offset, SIROffset::Static(0))
                            && full_width
                            && must_execute.contains(&block.id);
                        stores.push(StoreCandidate {
                            eu_index,
                            block: block.id,
                            instruction_index,
                            address: *address,
                            width: *width,
                            source: *source,
                            identity_source: shape_is_aliasable
                                .then(|| trace_identity_source(*source, *width, &defs))
                                .flatten(),
                            shape_is_aliasable,
                            effects_are_removable: triggers.is_empty()
                                && capture_sites.is_empty()
                                && !followed_by_capture,
                        });
                    }
                    SIRInstruction::Commit(source, destination, _, _, _) => {
                        increment_saturating(
                            &mut accesses.entry(source.absolute_addr()).or_default().reads,
                        );
                        increment_saturating(
                            &mut accesses
                                .entry(destination.absolute_addr())
                                .or_default()
                                .writes,
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    stores.sort_unstable_by_key(|store| {
        (
            store.eu_index,
            store.block,
            store.instruction_index,
            store.address,
        )
    });
    GlobalIdentityFacts { accesses, stores }
}

fn select_aliases(
    facts: &GlobalIdentityFacts,
    metadata: &HashMap<AbsoluteAddr, AddressMetadata>,
    blocked_aliases: &HashSet<AbsoluteAddr>,
    existing_aliases: &HashMap<AbsoluteAddr, AbsoluteAddr>,
    four_state: bool,
) -> HashMap<AbsoluteAddr, AbsoluteAddr> {
    let mut aliases = HashMap::default();

    // Store(B, identity_copy(A)): B must be globally unread and uniquely
    // written, while A must remain stable throughout eval_comb.
    for store in &facts.stores {
        let alias = store.address.absolute_addr();
        let Some(canonical_address) = store.identity_source else {
            continue;
        };
        let canonical = canonical_address.absolute_addr();
        if alias == canonical
            || !store.shape_is_aliasable
            || !store.effects_are_removable
            || blocked_aliases.contains(&alias)
            || existing_aliases.contains_key(&alias)
            || !is_unique_unread_alias(alias, &facts.accesses)
            || facts
                .accesses
                .get(&canonical)
                .is_some_and(|access| access.writes != 0)
            || canonical_address.region != STABLE_REGION
            || !storage_is_compatible(alias, canonical, metadata, four_state)
        {
            continue;
        }
        aliases.insert(alias, canonical);
    }

    // Store(A, src), Store(B, src): RegisterIds are local to an EU, and the
    // same block parameter can carry different values across loop iterations.
    // Requiring the same block proves that both stores execute together with
    // the same dynamic source value.  The removed side must additionally be
    // effect-free and globally unread.
    let mut duplicate_groups =
        HashMap::<(usize, BlockId, RegisterId, usize), Vec<usize>>::default();
    for (index, store) in facts.stores.iter().enumerate() {
        let address = store.address.absolute_addr();
        if store.shape_is_aliasable
            && facts
                .accesses
                .get(&address)
                .is_some_and(|access| access.writes == 1)
            && !aliases.contains_key(&address)
            && !existing_aliases.contains_key(&address)
        {
            duplicate_groups
                .entry((store.eu_index, store.block, store.source, store.width))
                .or_default()
                .push(index);
        }
    }

    for members in duplicate_groups.values_mut() {
        if members.len() < 2 {
            continue;
        }
        // Prefer an address that is read somewhere as the retained canonical;
        // it could not be the removable side.  Address order makes the choice
        // deterministic when the access facts are otherwise equal.
        members.sort_unstable_by_key(|&index| {
            let address = facts.stores[index].address.absolute_addr();
            let unread = facts
                .accesses
                .get(&address)
                .is_none_or(|access| access.reads == 0);
            (unread, address)
        });
        let canonical = facts.stores[members[0]].address.absolute_addr();
        for &index in members.iter().skip(1) {
            let store = facts.stores[index];
            let alias = store.address.absolute_addr();
            if alias == canonical
                || aliases.contains_key(&alias)
                || !store.effects_are_removable
                || blocked_aliases.contains(&alias)
                || !is_unique_unread_alias(alias, &facts.accesses)
                || !storage_is_compatible(alias, canonical, metadata, four_state)
            {
                continue;
            }
            aliases.insert(alias, canonical);
        }
    }

    aliases
}

fn is_unique_unread_alias(
    address: AbsoluteAddr,
    accesses: &HashMap<AbsoluteAddr, AddressFacts>,
) -> bool {
    accesses
        .get(&address)
        .is_some_and(|access| access.writes == 1 && access.reads == 0)
}

fn storage_is_compatible(
    alias: AbsoluteAddr,
    canonical: AbsoluteAddr,
    metadata: &HashMap<AbsoluteAddr, AddressMetadata>,
    four_state: bool,
) -> bool {
    metadata
        .get(&alias)
        .zip(metadata.get(&canonical))
        .is_some_and(|(alias_info, canonical_info)| {
            alias_info.width <= canonical_info.width
                && (!four_state || (!alias_info.is_4state && !canonical_info.is_4state))
        })
}

fn increment_saturating(value: &mut usize) {
    *value = value.saturating_add(1);
}

fn ff_addresses(program: &Program) -> HashSet<AbsoluteAddr> {
    let mut addresses = HashSet::default();
    let units = program
        .eval_apply_ffs
        .values()
        .flat_map(|units| units.iter())
        .chain(
            program
                .eval_only_ffs
                .values()
                .flat_map(|units| units.iter()),
        )
        .chain(program.apply_ffs.values().flat_map(|units| units.iter()));
    for eu in units {
        for block in eu.blocks.values() {
            for inst in &block.instructions {
                match inst {
                    SIRInstruction::Load(_, address, _, _)
                    | SIRInstruction::Store(address, _, _, _, _, _) => {
                        addresses.insert(address.absolute_addr());
                    }
                    SIRInstruction::Commit(source, destination, _, _, _) => {
                        addresses.insert(source.absolute_addr());
                        addresses.insert(destination.absolute_addr());
                    }
                    _ => {}
                }
            }
        }
    }
    addresses
}

/// Return the blocks that every terminating path from the entry executes.
/// This is the ancestor chain of the nearest common dominator of all reachable
/// Return/Error blocks.
fn must_execute_blocks(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashSet<BlockId> {
    let blocks = reverse_postorder(eu);
    if blocks.is_empty() {
        return HashSet::default();
    }
    let indices = blocks
        .iter()
        .copied()
        .enumerate()
        .map(|(index, block)| (block, index))
        .collect::<HashMap<_, _>>();
    let mut predecessors = vec![Vec::new(); blocks.len()];
    let mut terminals = Vec::new();
    for (index, block_id) in blocks.iter().copied().enumerate() {
        let Some(block) = eu.blocks.get(&block_id) else {
            return HashSet::default();
        };
        let successors = terminator_successors(&block.terminator);
        if successors.is_empty() {
            terminals.push(index);
        }
        for successor in successors {
            let Some(&successor_index) = indices.get(&successor) else {
                return HashSet::default();
            };
            predecessors[successor_index].push(index);
        }
    }
    let Some((&first_terminal, remaining_terminals)) = terminals.split_first() else {
        return HashSet::default();
    };

    let mut immediate_dominator = vec![None; blocks.len()];
    immediate_dominator[0] = Some(0);
    let mut changed = true;
    while changed {
        changed = false;
        for block in 1..blocks.len() {
            let mut defined_predecessors = predecessors[block]
                .iter()
                .copied()
                .filter(|&predecessor| immediate_dominator[predecessor].is_some());
            let Some(mut new_dominator) = defined_predecessors.next() else {
                continue;
            };
            for predecessor in defined_predecessors {
                let Some(intersection) =
                    intersect_dominators(predecessor, new_dominator, &immediate_dominator)
                else {
                    return HashSet::default();
                };
                new_dominator = intersection;
            }
            if immediate_dominator[block] != Some(new_dominator) {
                immediate_dominator[block] = Some(new_dominator);
                changed = true;
            }
        }
    }

    let mut common_dominator = first_terminal;
    for &terminal in remaining_terminals {
        let Some(intersection) =
            intersect_dominators(common_dominator, terminal, &immediate_dominator)
        else {
            return HashSet::default();
        };
        common_dominator = intersection;
    }

    let mut result = HashSet::default();
    loop {
        result.insert(blocks[common_dominator]);
        let Some(parent) = immediate_dominator[common_dominator] else {
            return HashSet::default();
        };
        if parent == common_dominator {
            break;
        }
        common_dominator = parent;
    }
    result
}

fn intersect_dominators(
    mut lhs: usize,
    mut rhs: usize,
    immediate_dominator: &[Option<usize>],
) -> Option<usize> {
    while lhs != rhs {
        while lhs > rhs {
            lhs = *immediate_dominator.get(lhs)?.as_ref()?;
        }
        while rhs > lhs {
            rhs = *immediate_dominator.get(rhs)?.as_ref()?;
        }
    }
    Some(lhs)
}

fn reverse_postorder(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> Vec<BlockId> {
    if !eu.blocks.contains_key(&eu.entry_block_id) {
        return Vec::new();
    }
    let mut seen = HashSet::default();
    let mut postorder = Vec::new();
    let mut stack = vec![(eu.entry_block_id, false)];
    while let Some((block_id, expanded)) = stack.pop() {
        if expanded {
            postorder.push(block_id);
            continue;
        }
        if !seen.insert(block_id) {
            continue;
        }
        let Some(block) = eu.blocks.get(&block_id) else {
            return Vec::new();
        };
        stack.push((block_id, true));
        let mut successors = terminator_successors(&block.terminator);
        successors.reverse();
        stack.extend(successors.into_iter().map(|successor| (successor, false)));
    }
    postorder.reverse();
    postorder
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

/// Trace a register to determine if it's an identity copy of some address.
/// Returns the source address if the value is a bit-for-bit copy.
fn trace_identity_source(
    reg: RegisterId,
    expected_width: usize,
    defs: &HashMap<RegisterId, &SIRInstruction<RegionedAbsoluteAddr>>,
) -> Option<RegionedAbsoluteAddr> {
    if expected_width == 0 {
        return None;
    }
    let def = defs.get(&reg).copied()?;

    match def {
        // Direct Load: Store(B, Load(A)) where widths match
        SIRInstruction::Load(_, addr, SIROffset::Static(0), width) if *width == expected_width => {
            Some(*addr)
        }

        // Concat of sequential 1-bit Loads from same address (MSB first)
        SIRInstruction::Concat(_, args) if args.len() == expected_width => {
            trace_concat_identity(args, expected_width, defs)
        }

        // Concat of multi-bit chunks that together form an identity copy.
        // Pattern: Load(A,W) → Shr+And per chunk → Concat → Store(B,W)
        // (generated by atomize_logic_paths for array port glue)
        SIRInstruction::Concat(_, args) => trace_concat_chunks_identity(args, expected_width, defs),

        // Look through identity/cast
        SIRInstruction::Unary(_, UnaryOp::Ident, inner) => {
            trace_identity_source(*inner, expected_width, defs)
        }

        _ => None,
    }
}

/// Check if a Concat of multi-bit chunks forms an identity copy.
///
/// Pattern (generated by atomize for array port glue):
///   r0 = Load(A, 0, W)
///   r_lo = (r0 >> 0) & mask_lo
///   r_hi = (r0 >> K) & mask_hi
///   result = Concat([r_hi, r_lo])   // MSB first
fn trace_concat_chunks_identity(
    args: &[RegisterId],
    expected_width: usize,
    defs: &HashMap<RegisterId, &SIRInstruction<RegionedAbsoluteAddr>>,
) -> Option<RegionedAbsoluteAddr> {
    let mut chunks: Vec<(RegionedAbsoluteAddr, usize, usize)> = Vec::new();
    for &arg in args {
        chunks.push(trace_chunk_source(arg, defs)?);
    }

    let source_addr = chunks.first()?.0;
    if !chunks.iter().all(|(address, _, _)| *address == source_addr) {
        return None;
    }

    // Concat is MSB-first; chunks in reverse should tile [0, expected_width).
    let mut total_width = 0;
    for (_, offset, width) in chunks.iter().rev() {
        if *offset != total_width {
            return None;
        }
        total_width += width;
    }
    if total_width != expected_width {
        return None;
    }

    Some(source_addr)
}

/// Trace a single chunk: (value >> shift) & mask, where value = Load(A).
fn trace_chunk_source(
    reg: RegisterId,
    defs: &HashMap<RegisterId, &SIRInstruction<RegionedAbsoluteAddr>>,
) -> Option<(RegionedAbsoluteAddr, usize, usize)> {
    let def = defs.get(&reg).copied()?;
    match def {
        SIRInstruction::Load(_, addr, SIROffset::Static(offset), width) => {
            Some((*addr, *offset, *width))
        }
        // (src >> shift) & mask  or  src & mask  (shift=0)
        SIRInstruction::Binary(_, lhs, BinaryOp::And, mask_reg) => {
            let chunk_width = match defs.get(mask_reg).copied()? {
                SIRInstruction::Imm(_, v) => {
                    if !v.mask.to_u64_digits().is_empty() {
                        return None;
                    }
                    let bits = v.payload.bits() as usize;
                    if bits == 0 {
                        return None;
                    }
                    let expected =
                        (num_bigint::BigUint::from(1u32) << bits) - num_bigint::BigUint::from(1u32);
                    if v.payload != expected {
                        return None;
                    }
                    bits
                }
                _ => return None,
            };

            match defs.get(lhs).copied()? {
                SIRInstruction::Load(_, addr, SIROffset::Static(0), load_width)
                    if *load_width >= chunk_width =>
                {
                    Some((*addr, 0, chunk_width))
                }
                SIRInstruction::Binary(_, src, BinaryOp::Shr, shift_reg) => {
                    let shift = match defs.get(shift_reg).copied()? {
                        SIRInstruction::Imm(_, v) => sir_value_to_u64(v)? as usize,
                        _ => return None,
                    };
                    match defs.get(src).copied()? {
                        SIRInstruction::Load(_, addr, SIROffset::Static(0), load_width)
                            if shift
                                .checked_add(chunk_width)
                                .is_some_and(|end| end <= *load_width) =>
                        {
                            Some((*addr, shift, chunk_width))
                        }
                        _ => None,
                    }
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Check if a Concat's args form an identity copy: each arg at position i
/// loads bit (W-1-i) from the same address.
fn trace_concat_identity(
    args: &[RegisterId],
    width: usize,
    defs: &HashMap<RegisterId, &SIRInstruction<RegionedAbsoluteAddr>>,
) -> Option<RegionedAbsoluteAddr> {
    let mut source_addr: Option<RegionedAbsoluteAddr> = None;

    for (i, &arg) in args.iter().enumerate() {
        let expected_bit = width - 1 - i; // MSB first in Concat
        let arg_def = defs.get(&arg).copied()?;

        let (addr, bit) = match arg_def {
            // Direct 1-bit Load
            SIRInstruction::Load(_, addr, SIROffset::Static(offset), 1) => (*addr, *offset),

            // Bit extract: (Load(A) >> K) & 1
            SIRInstruction::Binary(_, shifted, BinaryOp::And, mask_reg) => {
                let mask_def = defs.get(mask_reg).copied()?;
                let SIRInstruction::Imm(_, mask_val) = mask_def else {
                    return None;
                };
                if sir_value_to_u64(mask_val)? != 1 {
                    return None;
                }

                match defs.get(shifted).copied()? {
                    SIRInstruction::Binary(_, src, BinaryOp::Shr, shift_reg) => {
                        let SIRInstruction::Imm(_, sv) = defs.get(shift_reg).copied()? else {
                            return None;
                        };
                        let shift = sir_value_to_u64(sv)? as usize;
                        // src must be a Load
                        let SIRInstruction::Load(_, addr, SIROffset::Static(0), load_width) =
                            defs.get(src).copied()?
                        else {
                            return None;
                        };
                        if shift >= *load_width {
                            return None;
                        }
                        (*addr, shift)
                    }
                    // No shift: bit 0
                    SIRInstruction::Load(_, addr, SIROffset::Static(0), load_width)
                        if *load_width >= 1 =>
                    {
                        (*addr, 0)
                    }
                    _ => return None,
                }
            }

            _ => return None,
        };

        if bit != expected_bit {
            return None;
        }

        match &source_addr {
            Some(a) if *a != addr => return None,
            None => source_addr = Some(addr),
            _ => {}
        }
    }

    source_addr
}

#[cfg(test)]
mod tests {
    use super::*;
    use veryl_analyzer::ir::VarId;

    fn var_id(index: usize) -> VarId {
        let mut id = VarId::default();
        for _ in 0..index {
            id.inc();
        }
        id
    }

    fn address(index: usize) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: InstanceId(0),
            var_id: var_id(index),
        }
    }

    fn register_map(registers: &[(usize, usize)]) -> HashMap<RegisterId, RegisterType> {
        registers
            .iter()
            .map(|&(register, width)| {
                (
                    RegisterId(register),
                    RegisterType::Bit {
                        width,
                        signed: false,
                    },
                )
            })
            .collect()
    }

    fn single_block_unit(
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
        registers: &[(usize, usize)],
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let block = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions,
            terminator: SIRTerminator::Return,
        };
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [(BlockId(0), block)].into_iter().collect(),
            register_map: register_map(registers),
        };
        assert!(unit.verify_result().is_ok());
        unit
    }

    fn load(
        register: usize,
        address: RegionedAbsoluteAddr,
        width: usize,
    ) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Load(RegisterId(register), address, SIROffset::Static(0), width)
    }

    fn store(
        address: RegionedAbsoluteAddr,
        register: usize,
        width: usize,
    ) -> SIRInstruction<RegionedAbsoluteAddr> {
        SIRInstruction::Store(
            address,
            SIROffset::Static(0),
            width,
            RegisterId(register),
            Vec::new(),
            Vec::new(),
        )
    }

    fn metadata(
        entries: &[(RegionedAbsoluteAddr, usize)],
    ) -> HashMap<AbsoluteAddr, AddressMetadata> {
        entries
            .iter()
            .map(|&(address, width)| {
                (
                    address.absolute_addr(),
                    AddressMetadata {
                        width,
                        is_4state: false,
                    },
                )
            })
            .collect()
    }

    fn aliases(
        units: &[ExecutionUnit<RegionedAbsoluteAddr>],
        metadata: &HashMap<AbsoluteAddr, AddressMetadata>,
    ) -> HashMap<AbsoluteAddr, AbsoluteAddr> {
        analyze_eval_comb_aliases(
            units,
            metadata,
            &HashSet::default(),
            &HashMap::default(),
            false,
        )
    }

    #[test]
    fn identity_store_in_common_merge_block_is_must_execute() {
        let source = address(0);
        let destination = address(1);
        let entry = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                load(1, source, 8),
            ],
            terminator: SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        };
        let true_block = BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
        };
        let false_block = BasicBlock {
            id: BlockId(2),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Jump(BlockId(3), Vec::new()),
        };
        let merge = BasicBlock {
            id: BlockId(3),
            params: Vec::new(),
            instructions: vec![store(destination, 1, 8)],
            terminator: SIRTerminator::Return,
        };
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [entry, true_block, false_block, merge]
                .into_iter()
                .map(|block| (block.id, block))
                .collect(),
            register_map: register_map(&[(0, 1), (1, 8)]),
        };
        assert!(unit.verify_result().is_ok());

        assert_eq!(
            aliases(&[unit], &metadata(&[(source, 8), (destination, 8)]))
                .get(&destination.absolute_addr()),
            Some(&source.absolute_addr())
        );
    }

    #[test]
    fn conditional_identity_store_is_not_an_alias() {
        let source = address(0);
        let destination = address(1);
        let entry = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8)),
                load(1, source, 8),
            ],
            terminator: SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        };
        let true_block = BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: vec![store(destination, 1, 8)],
            terminator: SIRTerminator::Return,
        };
        let false_block = BasicBlock {
            id: BlockId(2),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Return,
        };
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [entry, true_block, false_block]
                .into_iter()
                .map(|block| (block.id, block))
                .collect(),
            register_map: register_map(&[(0, 1), (1, 8)]),
        };
        assert!(unit.verify_result().is_ok());

        assert!(aliases(&[unit], &metadata(&[(source, 8), (destination, 8)])).is_empty());
    }

    #[test]
    fn second_unit_load_blocks_identity_alias() {
        let source = address(0);
        let destination = address(1);
        let writer = single_block_unit(
            vec![load(0, source, 8), store(destination, 0, 8)],
            &[(0, 8)],
        );
        let reader = single_block_unit(vec![load(0, destination, 8)], &[(0, 8)]);

        assert!(
            aliases(
                &[writer, reader],
                &metadata(&[(source, 8), (destination, 8)])
            )
            .is_empty()
        );
    }

    #[test]
    fn second_unit_write_blocks_global_unique_store_proof() {
        let source = address(0);
        let destination = address(1);
        let identity_writer = single_block_unit(
            vec![load(0, source, 8), store(destination, 0, 8)],
            &[(0, 8)],
        );
        let other_writer = single_block_unit(
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(7u8)),
                store(destination, 0, 8),
            ],
            &[(0, 8)],
        );

        assert!(
            aliases(
                &[identity_writer, other_writer],
                &metadata(&[(source, 8), (destination, 8)])
            )
            .is_empty()
        );
    }

    #[test]
    fn same_register_number_in_different_units_is_not_a_duplicate_value() {
        let lhs = address(0);
        let rhs = address(1);
        let lhs_writer = single_block_unit(
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(3u8)),
                store(lhs, 0, 8),
            ],
            &[(0, 8)],
        );
        let rhs_writer = single_block_unit(
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(9u8)),
                store(rhs, 0, 8),
            ],
            &[(0, 8)],
        );

        assert!(aliases(&[lhs_writer, rhs_writer], &metadata(&[(lhs, 8), (rhs, 8)])).is_empty());
    }

    #[test]
    fn duplicate_stores_in_different_blocks_are_not_assumed_lockstep() {
        let lhs = address(0);
        let rhs = address(1);
        let entry = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(3u8)),
                store(lhs, 0, 8),
            ],
            terminator: SIRTerminator::Jump(BlockId(1), Vec::new()),
        };
        let next = BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: vec![store(rhs, 0, 8)],
            terminator: SIRTerminator::Return,
        };
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [entry, next]
                .into_iter()
                .map(|block| (block.id, block))
                .collect(),
            register_map: register_map(&[(0, 8)]),
        };
        assert!(unit.verify_result().is_ok());

        assert!(aliases(&[unit], &metadata(&[(lhs, 8), (rhs, 8)])).is_empty());
    }

    #[test]
    fn unconditional_duplicate_stores_alias_only_the_globally_unread_side() {
        let lhs = address(0);
        let rhs = address(1);
        let writer = single_block_unit(
            vec![
                SIRInstruction::Imm(RegisterId(0), SIRValue::new(3u8)),
                store(lhs, 0, 8),
                store(rhs, 0, 8),
            ],
            &[(0, 8)],
        );
        let rhs_reader = single_block_unit(vec![load(0, rhs, 8)], &[(0, 8)]);

        let result = aliases(&[writer, rhs_reader], &metadata(&[(lhs, 8), (rhs, 8)]));
        assert_eq!(result.get(&lhs.absolute_addr()), Some(&rhs.absolute_addr()));
        assert!(!result.contains_key(&rhs.absolute_addr()));
    }

    #[test]
    fn conditional_duplicate_stores_are_not_aliased() {
        let lhs = address(0);
        let rhs = address(1);
        let entry = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![SIRInstruction::Imm(RegisterId(0), SIRValue::new(1u8))],
            terminator: SIRTerminator::Branch {
                cond: RegisterId(0),
                true_block: (BlockId(1), Vec::new()),
                false_block: (BlockId(2), Vec::new()),
            },
        };
        let true_block = BasicBlock {
            id: BlockId(1),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Imm(RegisterId(1), SIRValue::new(5u8)),
                store(lhs, 1, 8),
                store(rhs, 1, 8),
            ],
            terminator: SIRTerminator::Return,
        };
        let false_block = BasicBlock {
            id: BlockId(2),
            params: Vec::new(),
            instructions: Vec::new(),
            terminator: SIRTerminator::Return,
        };
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks: [entry, true_block, false_block]
                .into_iter()
                .map(|block| (block.id, block))
                .collect(),
            register_map: register_map(&[(0, 1), (1, 8)]),
        };
        assert!(unit.verify_result().is_ok());

        assert!(aliases(&[unit], &metadata(&[(lhs, 8), (rhs, 8)])).is_empty());
    }

    #[test]
    fn partial_and_capture_enabled_stores_are_not_removable_aliases() {
        let source = address(0);
        let partial_destination = address(1);
        let captured_destination = address(2);
        let partial = single_block_unit(
            vec![load(0, source, 4), store(partial_destination, 0, 4)],
            &[(0, 4)],
        );
        let captured = single_block_unit(
            vec![
                load(0, source, 8),
                store(captured_destination, 0, 8),
                SIRInstruction::CombCaptureEnableIfChanged {
                    old: RegisterId(0),
                    new: RegisterId(0),
                    sites: vec![1],
                },
            ],
            &[(0, 8)],
        );

        assert!(
            aliases(
                &[partial, captured],
                &metadata(&[
                    (source, 8),
                    (partial_destination, 8),
                    (captured_destination, 8),
                ])
            )
            .is_empty()
        );
    }
}
