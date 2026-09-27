//! Global value numbering with memory versions and liveness checks.

use super::*;

// ────────────────────────────────────────────────────────────────
// Global GVN (Global Value Numbering)
// ────────────────────────────────────────────────────────────────
//
// Dominator-tree-scoped CSE: walk blocks in dominator-tree pre-order,
// maintaining a scoped hash table. Entries from a dominator are visible
// to all dominated blocks, enabling cross-block redundancy elimination.

type ValueNumber = u32;

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
enum GvnOpcode {
    Add,
    Sub,
    Mul,
    MulImm,
    MulImm32,
    UMulHi,
    And,
    Or,
    Xor,
    Shr,
    Shl,
    Sar,
    AndImm,
    AndImm32,
    OrImm,
    ShrImm,
    ShlImm,
    SarImm,
    AddImm,
    SubImm,
    UDiv,
    URem,
    SDiv,
    SRem,
    BitNot,
    Neg,
    Popcnt,
    BsrOr,
    Pext,
    Pdep,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(super) enum GvnMemoryVariable {
    UnknownAll,
    UnknownBase(BaseReg),
    Byte(BaseReg, i64),
}

/// Structural identity of one reaching physical-memory definition. Phi
/// versions distinguish loop iterations and joining paths without depending
/// on hash-table iteration order or on unrelated tracked byte ranges.
#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
pub(super) enum GvnMemoryVersion {
    Entry(GvnMemoryVariable),
    Write {
        ordinal: usize,
        variable: GvnMemoryVariable,
    },
    Phi {
        block: usize,
        variable: GvnMemoryVariable,
    },
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub(super) struct GvnLoadVersion {
    unknown_all: GvnMemoryVersion,
    unknown_base: GvnMemoryVersion,
    pub(super) bytes: Box<[GvnMemoryVersion]>,
}

/// An expression over value numbers, not source VRegs.  Two different VRegs
/// that GVN has already proven equal therefore form the same later expression.
#[derive(Debug, Hash, PartialEq, Eq, Clone)]
enum GvnKey {
    Constant(u64),
    ConstantTable(ConstantTableId),
    Binary(GvnOpcode, ValueNumber, ValueNumber),
    BinaryImmU64(GvnOpcode, ValueNumber, u64),
    BinaryImmI32(GvnOpcode, ValueNumber, i32),
    ShiftImm(GvnOpcode, ValueNumber, u8),
    Unary(GvnOpcode, ValueNumber),
    Cmp(ValueNumber, ValueNumber, CmpKind),
    CmpImm(ValueNumber, i32, CmpKind),
    Select(ValueNumber, ValueNumber, ValueNumber),
    CmpSelect(ValueNumber, ValueNumber, CmpKind, ValueNumber, ValueNumber),
    CmpImmSelect(ValueNumber, i32, CmpKind, ValueNumber, ValueNumber),
    GuardedCmpSelect(
        ValueNumber,
        ValueNumber,
        ValueNumber,
        CmpKind,
        ValueNumber,
        ValueNumber,
    ),
    Load(BaseReg, i32, OpSize, GvnLoadVersion),
}

fn gvn_is_commutative(op: GvnOpcode) -> bool {
    matches!(
        op,
        GvnOpcode::Add
            | GvnOpcode::Mul
            | GvnOpcode::UMulHi
            | GvnOpcode::And
            | GvnOpcode::Or
            | GvnOpcode::Xor
    )
}

/// Whether allocation has target-level recovery choices for a same-block GVN
/// leader whose range is extended.
///
/// The one-source operations have exact allocator rematerialization recipes.
/// A SimState load instead has a versioned MemorySSA recipe at each valid use;
/// if a later write makes that recipe invalid, ordinary register or stack
/// residency remains available. GVN may therefore expose one shared value
/// even when its old leader has no later source-order use: pressure scheduling
/// can place users together, and allocation retains carry/split/home choices.
/// This is a cost freedom, not an ordering constraint.
fn allocator_can_recover_extended_gvn_leader(inst: &MInst) -> bool {
    matches!(
        inst,
        MInst::AndImm { .. }
            | MInst::MulImm { .. }
            | MInst::MulImm32 { .. }
            | MInst::AndImm32 { .. }
            | MInst::OrImm { .. }
            | MInst::ShrImm { .. }
            | MInst::ShlImm { .. }
            | MInst::SarImm { .. }
            | MInst::AddImm { .. }
            | MInst::SubImm { .. }
            | MInst::CmpImm { .. }
            | MInst::BitNot { .. }
            | MInst::Neg { .. }
            | MInst::Load {
                base: BaseReg::SimState,
                ..
            }
    )
}

fn gvn_value(value_numbers: &[ValueNumber], vreg: VReg) -> ValueNumber {
    value_numbers[vreg.0 as usize]
}

fn gvn_key(
    inst: &MInst,
    value_numbers: &[ValueNumber],
    load_version: Option<&GvnLoadVersion>,
) -> Option<GvnKey> {
    let value = |vreg| gvn_value(value_numbers, vreg);
    let binary = |op, lhs, rhs| gvn_binary(op, value(lhs), value(rhs));
    match inst {
        MInst::LoadImm { value, .. } => Some(GvnKey::Constant(*value)),
        MInst::LoadConstantTableAddr { table, .. } => Some(GvnKey::ConstantTable(*table)),
        MInst::Add { lhs, rhs, .. } => Some(binary(GvnOpcode::Add, *lhs, *rhs)),
        MInst::Sub { lhs, rhs, .. } => Some(binary(GvnOpcode::Sub, *lhs, *rhs)),
        MInst::Mul { lhs, rhs, .. } => Some(binary(GvnOpcode::Mul, *lhs, *rhs)),
        MInst::MulImm { src, imm, .. } => {
            Some(GvnKey::BinaryImmI32(GvnOpcode::MulImm, value(*src), *imm))
        }
        MInst::MulImm32 { src, imm, .. } => {
            Some(GvnKey::BinaryImmI32(GvnOpcode::MulImm32, value(*src), *imm))
        }
        MInst::UMulHi { lhs, rhs, .. } => Some(binary(GvnOpcode::UMulHi, *lhs, *rhs)),
        MInst::And { lhs, rhs, .. } => Some(binary(GvnOpcode::And, *lhs, *rhs)),
        MInst::Or { lhs, rhs, .. } => Some(binary(GvnOpcode::Or, *lhs, *rhs)),
        MInst::Xor { lhs, rhs, .. } => Some(binary(GvnOpcode::Xor, *lhs, *rhs)),
        MInst::Shr { lhs, rhs, .. } => Some(binary(GvnOpcode::Shr, *lhs, *rhs)),
        MInst::Shl { lhs, rhs, .. } => Some(binary(GvnOpcode::Shl, *lhs, *rhs)),
        MInst::Sar { lhs, rhs, .. } => Some(binary(GvnOpcode::Sar, *lhs, *rhs)),
        MInst::UDiv { lhs, rhs, .. } => Some(binary(GvnOpcode::UDiv, *lhs, *rhs)),
        MInst::URem { lhs, rhs, .. } => Some(binary(GvnOpcode::URem, *lhs, *rhs)),
        MInst::SDiv { lhs, rhs, .. } => Some(binary(GvnOpcode::SDiv, *lhs, *rhs)),
        MInst::SRem { lhs, rhs, .. } => Some(binary(GvnOpcode::SRem, *lhs, *rhs)),
        MInst::AndImm { src, imm, .. } => {
            Some(GvnKey::BinaryImmU64(GvnOpcode::AndImm, value(*src), *imm))
        }
        MInst::AndImm32 { src, imm, .. } => Some(GvnKey::BinaryImmU64(
            GvnOpcode::AndImm32,
            value(*src),
            u64::from(*imm),
        )),
        MInst::OrImm { src, imm, .. } => {
            Some(GvnKey::BinaryImmU64(GvnOpcode::OrImm, value(*src), *imm))
        }
        MInst::ShrImm { src, imm, .. } => {
            Some(GvnKey::ShiftImm(GvnOpcode::ShrImm, value(*src), *imm))
        }
        MInst::ShlImm { src, imm, .. } => {
            Some(GvnKey::ShiftImm(GvnOpcode::ShlImm, value(*src), *imm))
        }
        MInst::SarImm { src, imm, .. } => {
            Some(GvnKey::ShiftImm(GvnOpcode::SarImm, value(*src), *imm))
        }
        MInst::AddImm { src, imm, .. } => {
            Some(GvnKey::BinaryImmI32(GvnOpcode::AddImm, value(*src), *imm))
        }
        MInst::SubImm { src, imm, .. } => {
            Some(GvnKey::BinaryImmI32(GvnOpcode::SubImm, value(*src), *imm))
        }
        MInst::BitNot { src, .. } => Some(GvnKey::Unary(GvnOpcode::BitNot, value(*src))),
        MInst::Neg { src, .. } => Some(GvnKey::Unary(GvnOpcode::Neg, value(*src))),
        MInst::Popcnt { src, .. } => Some(GvnKey::Unary(GvnOpcode::Popcnt, value(*src))),
        // Unchecked bit scans have an unspecified result for zero, so they
        // have no reusable value.
        MInst::Bsf { .. } | MInst::Bsr { .. } => None,
        MInst::BsrOr {
            src, zero_value, ..
        } => Some(GvnKey::BinaryImmU64(
            GvnOpcode::BsrOr,
            value(*src),
            u64::from(*zero_value),
        )),
        MInst::Pext { src, mask, .. } => Some(binary(GvnOpcode::Pext, *src, *mask)),
        MInst::Pdep { src, mask, .. } => Some(binary(GvnOpcode::Pdep, *src, *mask)),
        MInst::Cmp { lhs, rhs, kind, .. } => {
            let (mut lhs, mut rhs) = (value(*lhs), value(*rhs));
            if matches!(kind, CmpKind::Eq | CmpKind::Ne) && rhs < lhs {
                std::mem::swap(&mut lhs, &mut rhs);
            }
            Some(GvnKey::Cmp(lhs, rhs, *kind))
        }
        MInst::CmpImm { lhs, imm, kind, .. } => Some(GvnKey::CmpImm(value(*lhs), *imm, *kind)),
        MInst::Select {
            cond,
            true_val,
            false_val,
            ..
        } => Some(GvnKey::Select(
            value(*cond),
            value(*true_val),
            value(*false_val),
        )),
        MInst::CmpSelect {
            lhs,
            rhs,
            kind,
            true_val,
            false_val,
            ..
        } => Some(GvnKey::CmpSelect(
            value(*lhs),
            value(*rhs),
            *kind,
            value(*true_val),
            value(*false_val),
        )),
        MInst::CmpImmSelect {
            lhs,
            imm,
            kind,
            true_val,
            false_val,
            ..
        } => Some(GvnKey::CmpImmSelect(
            value(*lhs),
            *imm,
            *kind,
            value(*true_val),
            value(*false_val),
        )),
        MInst::GuardedCmpSelect {
            guard,
            lhs,
            rhs,
            kind,
            true_val,
            false_val,
            ..
        } => Some(GvnKey::GuardedCmpSelect(
            value(*guard),
            value(*lhs),
            value(*rhs),
            *kind,
            value(*true_val),
            value(*false_val),
        )),
        MInst::Load {
            base, offset, size, ..
        } => Some(GvnKey::Load(*base, *offset, *size, load_version?.clone())),
        _ => None,
    }
}

fn gvn_binary(op: GvnOpcode, mut lhs: ValueNumber, mut rhs: ValueNumber) -> GvnKey {
    if gvn_is_commutative(op) && rhs < lhs {
        std::mem::swap(&mut lhs, &mut rhs);
    }
    GvnKey::Binary(op, lhs, rhs)
}

#[derive(Default)]
struct GvnTrackedMemory {
    sim_state: BTreeSet<i64>,
    stack_frame: BTreeSet<i64>,
}

impl GvnTrackedMemory {
    fn bytes(&self, base: BaseReg) -> &BTreeSet<i64> {
        match base {
            BaseReg::SimState => &self.sim_state,
            BaseReg::StackFrame => &self.stack_frame,
        }
    }

    fn bytes_mut(&mut self, base: BaseReg) -> &mut BTreeSet<i64> {
        match base {
            BaseReg::SimState => &mut self.sim_state,
            BaseReg::StackFrame => &mut self.stack_frame,
        }
    }

    fn tracks_base(&self, base: BaseReg) -> bool {
        !self.bytes(base).is_empty()
    }
}

fn gvn_memory_variable_key(variable: GvnMemoryVariable) -> (u8, i64) {
    match variable {
        GvnMemoryVariable::UnknownAll => (0, 0),
        GvnMemoryVariable::UnknownBase(BaseReg::SimState) => (1, 0),
        GvnMemoryVariable::UnknownBase(BaseReg::StackFrame) => (2, 0),
        GvnMemoryVariable::Byte(BaseReg::SimState, byte) => (3, byte),
        GvnMemoryVariable::Byte(BaseReg::StackFrame, byte) => (4, byte),
    }
}

fn gvn_memory_version(
    variable: GvnMemoryVariable,
    current: &HashMap<GvnMemoryVariable, GvnMemoryVersion>,
) -> GvnMemoryVersion {
    current
        .get(&variable)
        .copied()
        .unwrap_or(GvnMemoryVersion::Entry(variable))
}

fn gvn_load_version(
    base: BaseReg,
    offset: i32,
    size: OpSize,
    current: &HashMap<GvnMemoryVariable, GvnMemoryVersion>,
) -> Option<GvnLoadVersion> {
    let start = i64::from(offset);
    let end = start.checked_add(i64::from(size.bytes()))?;
    Some(GvnLoadVersion {
        unknown_all: gvn_memory_version(GvnMemoryVariable::UnknownAll, current),
        unknown_base: gvn_memory_version(GvnMemoryVariable::UnknownBase(base), current),
        bytes: (start..end)
            .map(|byte| gvn_memory_version(GvnMemoryVariable::Byte(base, byte), current))
            .collect(),
    })
}

fn gvn_affected_memory_variables(
    effect: &memory_effect::MemoryEffects,
    tracked: &GvnTrackedMemory,
) -> Option<Vec<GvnMemoryVariable>> {
    if let Some(memory) = effect.unknown_memory() {
        return Some(match memory {
            memory_effect::UnknownMemory::Direct(base) if tracked.tracks_base(base) => {
                vec![GvnMemoryVariable::UnknownBase(base)]
            }
            memory_effect::UnknownMemory::Direct(_) | memory_effect::UnknownMemory::Indirect => {
                Vec::new()
            }
        });
    }
    let mut affected = HashSet::<GvnMemoryVariable>::default();
    for range in effect.ranges() {
        let end = range.end()?;
        affected.extend(
            tracked
                .bytes(range.base)
                .range(range.offset..end)
                .copied()
                .map(|byte| GvnMemoryVariable::Byte(range.base, byte)),
        );
    }
    let mut affected = affected.into_iter().collect::<Vec<_>>();
    affected.sort_unstable_by_key(|variable| gvn_memory_variable_key(*variable));
    Some(affected)
}

fn gvn_dominance_frontiers(
    predecessors: &[Vec<usize>],
    idom: &[Option<usize>],
) -> Option<Vec<BTreeSet<usize>>> {
    if predecessors.len() != idom.len() {
        return None;
    }
    let mut frontiers = vec![BTreeSet::new(); predecessors.len()];
    for (block, incoming) in predecessors.iter().enumerate() {
        if incoming.len() < 2 {
            continue;
        }
        let immediate = idom[block]?;
        for &predecessor in incoming {
            let mut runner = predecessor;
            let mut steps = 0usize;
            while runner != immediate {
                frontiers.get_mut(runner)?.insert(block);
                runner = idom.get(runner).copied().flatten()?;
                steps = steps.checked_add(1)?;
                if steps > idom.len() {
                    return None;
                }
            }
        }
    }
    Some(frontiers)
}

/// Build sparse byte-granular MemorySSA versions for every exact MIR load.
/// The result is keyed by the original block/instruction location and is
/// computed before GVN mutates any instruction into a copy.
pub(super) fn compute_gvn_load_versions(
    func: &MFunction,
    predecessors: &[Vec<usize>],
    idom: &[Option<usize>],
) -> Option<HashMap<(usize, usize), GvnLoadVersion>> {
    let mut tracked = GvnTrackedMemory::default();
    for block in &func.blocks {
        for inst in &block.insts {
            let MInst::Load {
                base, offset, size, ..
            } = inst
            else {
                continue;
            };
            let start = i64::from(*offset);
            let end = start.checked_add(i64::from(size.bytes()))?;
            tracked.bytes_mut(*base).extend(start..end);
        }
    }
    if tracked.sim_state.is_empty() && tracked.stack_frame.is_empty() {
        return Some(HashMap::default());
    }

    let frontiers = gvn_dominance_frontiers(predecessors, idom)?;
    let mut definition_blocks = HashMap::<GvnMemoryVariable, BTreeSet<usize>>::default();
    // A write's identity is its ordinal in the original instruction stream.
    // Keep one starting ordinal per block instead of repeating the full
    // version for every byte written by every instruction.
    let mut block_write_ordinals = Vec::with_capacity(func.blocks.len());
    let mut write_ordinal = 0usize;
    for (block, mir_block) in func.blocks.iter().enumerate() {
        block_write_ordinals.push(write_ordinal);
        for inst in &mir_block.insts {
            let effect = memory_effect::writes(inst);
            if effect.has_effect() {
                write_ordinal = write_ordinal.checked_add(1)?;
            }
            for variable in gvn_affected_memory_variables(&effect, &tracked)? {
                definition_blocks.entry(variable).or_default().insert(block);
            }
        }
    }

    let mut phis_by_block = vec![Vec::<GvnMemoryVariable>::new(); func.blocks.len()];
    for (variable, original_definitions) in definition_blocks {
        let mut definitions = original_definitions;
        let mut queue = definitions.iter().copied().collect::<VecDeque<_>>();
        let mut placed = BTreeSet::<usize>::new();
        while let Some(definition) = queue.pop_front() {
            for &frontier in &frontiers[definition] {
                if frontier == 0 || !placed.insert(frontier) {
                    continue;
                }
                phis_by_block[frontier].push(variable);
                if definitions.insert(frontier) {
                    queue.push_back(frontier);
                }
            }
        }
    }
    for phis in &mut phis_by_block {
        phis.sort_unstable_by_key(|variable| gvn_memory_variable_key(*variable));
    }

    let mut children = vec![Vec::<usize>::new(); func.blocks.len()];
    for (block, parent) in idom.iter().copied().enumerate().skip(1) {
        if let Some(parent) = parent {
            children[parent].push(block);
        }
    }

    enum Action {
        Enter(usize),
        Exit(Vec<(GvnMemoryVariable, Option<GvnMemoryVersion>)>),
    }
    let mut current = HashMap::<GvnMemoryVariable, GvnMemoryVersion>::default();
    let mut versions = HashMap::<(usize, usize), GvnLoadVersion>::default();
    let mut actions = vec![Action::Enter(0)];
    while let Some(action) = actions.pop() {
        let block = match action {
            Action::Exit(changes) => {
                for (variable, previous) in changes.into_iter().rev() {
                    if let Some(previous) = previous {
                        current.insert(variable, previous);
                    } else {
                        current.remove(&variable);
                    }
                }
                continue;
            }
            Action::Enter(block) => block,
        };
        let mut changes = Vec::new();
        for &variable in &phis_by_block[block] {
            let version = GvnMemoryVersion::Phi { block, variable };
            changes.push((variable, current.insert(variable, version)));
        }
        let mut write_ordinal = block_write_ordinals[block];
        for (instruction, inst) in func.blocks[block].insts.iter().enumerate() {
            let effect = memory_effect::writes(inst);
            let ordinal = if effect.has_effect() {
                let ordinal = write_ordinal;
                write_ordinal = write_ordinal.checked_add(1)?;
                Some(ordinal)
            } else {
                None
            };
            if let MInst::Load {
                base, offset, size, ..
            } = inst
            {
                versions.insert(
                    (block, instruction),
                    gvn_load_version(*base, *offset, *size, &current)?,
                );
            }
            for variable in gvn_affected_memory_variables(&effect, &tracked)? {
                let version = GvnMemoryVersion::Write {
                    ordinal: ordinal.expect("an affected variable belongs to a memory write"),
                    variable,
                };
                changes.push((variable, current.insert(variable, version)));
            }
        }
        actions.push(Action::Exit(changes));
        actions.extend(children[block].iter().rev().copied().map(Action::Enter));
    }
    Some(versions)
}

/// Global GVN: dominator-tree-scoped value numbering.
pub(super) fn global_gvn(func: &mut MFunction) {
    let num_blocks = func.blocks.len();
    if num_blocks == 0 {
        return;
    }

    // Build block index map: BlockId → index
    let block_id_to_idx: HashMap<BlockId, usize> = func
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id, i))
        .collect();

    // Build predecessor lists and successor lists
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); num_blocks];
    let mut succs: Vec<Vec<usize>> = vec![Vec::new(); num_blocks];
    for (i, block) in func.blocks.iter().enumerate() {
        for succ_id in block.successors() {
            if let Some(&j) = block_id_to_idx.get(&succ_id) {
                succs[i].push(j);
                preds[j].push(i);
            }
        }
    }

    // Compute dominators using simple iterative algorithm (Cooper, Harvey, Kennedy)
    let idom = compute_dominators(num_blocks, &preds, &succs);
    let load_versions = compute_gvn_load_versions(func, &preds, &idom).unwrap_or_default();
    let mut live_out = gvn_liveness::live_out(func, &block_id_to_idx, &preds);
    let last_uses = func
        .blocks
        .iter()
        .map(|block| {
            let mut uses = HashMap::default();
            for (instruction, inst) in block.insts.iter().enumerate() {
                for value in inst.uses() {
                    uses.insert(value, instruction);
                }
            }
            uses
        })
        .collect::<Vec<_>>();

    // Build dominator tree children
    let mut dom_children: Vec<Vec<usize>> = vec![Vec::new(); num_blocks];
    for (i, dom) in idom.iter().enumerate().skip(1) {
        if let Some(parent) = dom {
            dom_children[*parent].push(i);
        }
    }

    // Every VReg starts in its own value class.  Processing a copy or a
    // redundant expression merges the destination into an existing class.
    // Keeping this separate from the scoped expression table is what makes
    // this value numbering rather than an operand-identical CSE pass.
    let vreg_count = func.vregs.count() as usize;
    let mut value_numbers = (0..func.vregs.count()).collect::<Vec<ValueNumber>>();
    let mut value_leaders = (0..func.vregs.count()).map(VReg).collect::<Vec<_>>();
    let mut leader_blocks = vec![None; vreg_count];
    debug_assert_eq!(value_numbers.len(), vreg_count);

    let mut value_table: HashMap<GvnKey, ValueNumber> = HashMap::default();
    let mut table_changes: Vec<(GvnKey, Option<ValueNumber>)> = Vec::new();
    let mut leader_changes: Vec<(ValueNumber, VReg, Option<usize>)> = Vec::new();
    let mut replacements: Vec<(usize, usize, MInst)> = Vec::new(); // (block_idx, inst_idx, new_inst)

    // Dominator-scoped GVN. Every table mutation is undo-logged so sibling
    // subtrees see exactly the expression scope at their common dominator.
    // Load validity is carried by the structural MemorySSA version in its key,
    // rather than by a path-local store invalidation side table.
    fn process_gvn_block(
        node: usize,
        block: &MBlock,
        value_numbers: &mut [ValueNumber],
        value_leaders: &mut [VReg],
        leader_blocks: &mut [Option<usize>],
        live_out: &mut gvn_liveness::LiveOut<'_>,
        last_uses: &HashMap<VReg, usize>,
        load_versions: &HashMap<(usize, usize), GvnLoadVersion>,
        value_table: &mut HashMap<GvnKey, ValueNumber>,
        table_changes: &mut Vec<(GvnKey, Option<ValueNumber>)>,
        leader_changes: &mut Vec<(ValueNumber, VReg, Option<usize>)>,
        replacements: &mut Vec<(usize, usize, MInst)>,
    ) {
        for inst_idx in 0..block.insts.len() {
            let inst = &block.insts[inst_idx];

            if let MInst::Mov { dst, src } = inst {
                let number = gvn_value(value_numbers, *src);
                value_numbers[dst.0 as usize] = number;
                continue;
            }

            if let Some(key) = gvn_key(inst, value_numbers, load_versions.get(&(node, inst_idx))) {
                let dst = inst
                    .def()
                    .expect("every value-numbered MIR instruction must define a VReg");
                if let Some(&number) = value_table.get(&key) {
                    let leader = value_leaders[number as usize];
                    value_numbers[dst.0 as usize] = number;
                    let leader_block = leader_blocks[number as usize];
                    let reuse_does_not_extend_live_range = live_out.contains(leader, node)
                        || last_uses
                            .get(&leader)
                            .is_some_and(|last_use| *last_use >= inst_idx);
                    let allocator_can_choose = leader_block == Some(node)
                        && allocator_can_recover_extended_gvn_leader(inst);
                    if dst != leader && (reuse_does_not_extend_live_range || allocator_can_choose) {
                        replacements.push((node, inst_idx, MInst::Mov { dst, src: leader }));
                    } else if dst != leader {
                        // The expression is available, but reusing its original
                        // leader would keep that VReg alive solely for this CSE.
                        // Keep the recomputation and make it the nearest leader
                        // for the current dominator subtree instead.
                        leader_changes.push((number, leader, leader_block));
                        value_leaders[number as usize] = dst;
                        leader_blocks[number as usize] = Some(node);
                    }
                } else {
                    let number = value_numbers[dst.0 as usize];
                    leader_changes.push((
                        number,
                        value_leaders[number as usize],
                        leader_blocks[number as usize],
                    ));
                    value_leaders[number as usize] = dst;
                    leader_blocks[number as usize] = Some(node);
                    let previous = value_table.insert(key.clone(), number);
                    debug_assert!(previous.is_none());
                    table_changes.push((key, previous));
                }
            }
        }
    }

    enum GvnAction {
        Enter(usize),
        Exit {
            table_checkpoint: usize,
            leader_checkpoint: usize,
        },
    }
    // Deep dominator chains in large designs must not consume the compiler
    // thread's call stack. Preserve recursive DFS order and undo checkpoints.
    let mut actions = vec![GvnAction::Enter(0)];
    while let Some(action) = actions.pop() {
        let node = match action {
            GvnAction::Exit {
                table_checkpoint,
                leader_checkpoint,
            } => {
                while leader_changes.len() > leader_checkpoint {
                    let (number, leader, leader_block) = leader_changes.pop().unwrap();
                    value_leaders[number as usize] = leader;
                    leader_blocks[number as usize] = leader_block;
                }
                while table_changes.len() > table_checkpoint {
                    let (key, previous) = table_changes.pop().unwrap();
                    if let Some(previous) = previous {
                        value_table.insert(key, previous);
                    } else {
                        value_table.remove(&key);
                    }
                }
                continue;
            }
            GvnAction::Enter(node) => node,
        };
        actions.push(GvnAction::Exit {
            table_checkpoint: table_changes.len(),
            leader_checkpoint: leader_changes.len(),
        });
        process_gvn_block(
            node,
            &func.blocks[node],
            &mut value_numbers,
            &mut value_leaders,
            &mut leader_blocks,
            &mut live_out,
            &last_uses[node],
            &load_versions,
            &mut value_table,
            &mut table_changes,
            &mut leader_changes,
            &mut replacements,
        );
        actions.extend(
            dom_children[node]
                .iter()
                .rev()
                .copied()
                .map(GvnAction::Enter),
        );
    }
    debug_assert!(value_table.is_empty());
    debug_assert!(table_changes.is_empty());
    debug_assert!(leader_changes.is_empty());

    // Apply replacements
    for (bi, inst_idx, new_inst) in replacements {
        func.blocks[bi].insts[inst_idx] = new_inst;
    }
}
