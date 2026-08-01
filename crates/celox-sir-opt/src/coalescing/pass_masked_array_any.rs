//! Recover short-circuit control flow for a masked unpacked-array reduction.
//!
//! Vectorization deliberately turns lane-wise Boolean logic into packed masks,
//! but an equality leaf still has to read one unpacked-array element.  The
//! canonical eager form therefore reads and compares every lane:
//!
//! ```text
//! any(outer & (flags | (gate & concat(eq(load(array[i]), key), ...))))
//! ```
//!
//! In two-state logic this is exactly a search over the set bits of
//! `outer & gate`, after first checking `outer & flags`.  Recovering that
//! search keeps the useful packed-mask operations while moving the element
//! load and equality tests behind control dependence.  Source, layer, and
//! instruction order are not semantic constraints here; data/effect
//! dependence and the memory version read by each load are.

use super::pass_manager::ExecutionUnitPass;
use super::pass_vectorize_concat::remove_dead_definitions;
use super::shared::def_reg;
use crate::ir::*;
use crate::optimizer::PassOptions;
use crate::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ArrayShape {
    element_width: usize,
    element_count: usize,
}

#[derive(Clone, Default)]
pub(super) struct MaskedArrayAnyPass {
    unpacked_arrays: HashMap<AbsoluteAddr, ArrayShape>,
}

impl MaskedArrayAnyPass {
    pub(super) fn for_program(program: &Program) -> Self {
        let mut unpacked_arrays = HashMap::default();
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
            unpacked_arrays.insert(
                address,
                ArrayShape {
                    element_width: info.width / element_count,
                    element_count,
                },
            );
        }
        Self { unpacked_arrays }
    }
}

#[derive(Clone, Debug)]
struct ComparePack {
    concat: RegisterId,
    address: RegionedAbsoluteAddr,
    element_width: usize,
    element_type: RegisterType,
    loads_by_lane: Vec<RegisterId>,
    equality_defs: Vec<RegisterId>,
    constant: RegisterId,
    constant_value: SIRValue,
}

#[derive(Clone, Debug)]
struct CompareUnion {
    address: RegionedAbsoluteAddr,
    element_width: usize,
    element_type: RegisterType,
    lane_count: usize,
    constants: Vec<RegisterId>,
    loads_by_lane: Vec<RegisterId>,
    single_use_defs: Vec<RegisterId>,
}

#[derive(Clone, Debug)]
struct CandidateTerm {
    term: RegisterId,
    gate: RegisterId,
    comparisons: CompareUnion,
}

#[derive(Clone, Debug)]
struct RewritePlan {
    block: BlockId,
    root_index: usize,
    result: RegisterId,
    outer: RegisterId,
    gate: RegisterId,
    flags: Vec<RegisterId>,
    address: RegionedAbsoluteAddr,
    element_width: usize,
    element_type: RegisterType,
    lane_count: usize,
    constants: Vec<RegisterId>,
}

impl ExecutionUnitPass for MaskedArrayAnyPass {
    fn name(&self) -> &'static str {
        "masked_array_any"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        if options.four_state || self.unpacked_arrays.is_empty() {
            return;
        }

        let use_counts = register_use_counts(eu);
        let mut plans = Vec::new();
        let mut block_ids = eu.blocks.keys().copied().collect::<Vec<_>>();
        block_ids.sort_unstable();

        for block_id in block_ids {
            let block = &eu.blocks[&block_id];
            let definitions = block
                .instructions
                .iter()
                .enumerate()
                .filter_map(|(index, instruction)| def_reg(instruction).map(|reg| (reg, index)))
                .collect::<HashMap<_, _>>();
            for (root_index, instruction) in block.instructions.iter().enumerate() {
                let SIRInstruction::Unary(result, UnaryOp::Or, source) = instruction else {
                    continue;
                };
                let recognizer = Recognizer {
                    eu,
                    block,
                    root_index,
                    definitions: &definitions,
                    use_counts: &use_counts,
                    unpacked_arrays: &self.unpacked_arrays,
                };
                let Some(plan) = recognizer.recognize(*result, *source) else {
                    continue;
                };
                plans.push(plan);
            }
        }

        if plans.is_empty() {
            return;
        }

        // Splitting at a later root leaves every earlier instruction index in
        // the original block unchanged.  Applying roots in reverse order thus
        // realizes all plans from one O(input) discovery snapshot.
        plans.sort_unstable_by_key(|plan| (plan.block, std::cmp::Reverse(plan.root_index)));
        let mut next_block = eu.blocks.keys().map(|block| block.0).max().unwrap_or(0) + 1;
        let mut next_register = eu.register_map.keys().map(|reg| reg.0).max().unwrap_or(0);
        for plan in plans {
            apply_plan(eu, plan, &mut next_block, &mut next_register);
        }
        remove_dead_definitions(eu);
    }
}

struct Recognizer<'a> {
    eu: &'a ExecutionUnit<RegionedAbsoluteAddr>,
    block: &'a BasicBlock<RegionedAbsoluteAddr>,
    root_index: usize,
    definitions: &'a HashMap<RegisterId, usize>,
    use_counts: &'a HashMap<RegisterId, usize>,
    unpacked_arrays: &'a HashMap<AbsoluteAddr, ArrayShape>,
}

impl Recognizer<'_> {
    fn recognize(&self, result: RegisterId, source: RegisterId) -> Option<RewritePlan> {
        if register_width(self.eu, result)? != 1 {
            return None;
        }
        let SIRInstruction::Binary(_, lhs, BinaryOp::And, rhs) = self.definition(source)? else {
            return None;
        };

        let lhs_plan = self.recognize_orientation(result, source, *lhs, *rhs);
        let rhs_plan = self.recognize_orientation(result, source, *rhs, *lhs);
        match (lhs_plan, rhs_plan) {
            (Some(plan), None) | (None, Some(plan)) => Some(plan),
            // Do not choose an arbitrary interpretation when both operands
            // happen to have the canonical reduction shape.
            _ => None,
        }
    }

    fn recognize_orientation(
        &self,
        result: RegisterId,
        source: RegisterId,
        outer: RegisterId,
        body: RegisterId,
    ) -> Option<RewritePlan> {
        let width = unsigned_bit_width(self.eu, outer)?;
        if width < 2 || unsigned_bit_width(self.eu, body)? != width {
            return None;
        }

        let (terms, top_or_defs) = self.flatten_or(body, width)?;
        let mut candidate = None;
        let mut flags = Vec::new();
        for term in terms {
            if let Some(found) = self.match_candidate_term(term, width) {
                if candidate.is_some() {
                    return None;
                }
                candidate = Some(found);
            } else {
                if unsigned_bit_width(self.eu, term)? != width {
                    return None;
                }
                flags.push(term);
            }
        }
        let candidate = candidate?;
        if candidate.comparisons.lane_count != width
            || unsigned_bit_width(self.eu, candidate.gate)? != width
        {
            return None;
        }

        // Every eager array definition must disappear after replacement;
        // otherwise the search is added on top of still-live old work.  The
        // packed Boolean spine and each equality have one internal user, while
        // each shared lane load has one user per distinct key.
        let mut single_use_defs = vec![source, candidate.term];
        single_use_defs.extend(top_or_defs);
        single_use_defs.extend(candidate.comparisons.single_use_defs.iter().copied());
        single_use_defs.sort_unstable();
        single_use_defs.dedup();
        if single_use_defs
            .iter()
            .any(|reg| self.use_counts.get(reg).copied() != Some(1))
        {
            return None;
        }
        let key_count = candidate.comparisons.constants.len();
        if candidate
            .comparisons
            .loads_by_lane
            .iter()
            .any(|reg| self.use_counts.get(reg).copied() != Some(key_count))
        {
            return None;
        }

        // Compare generated static code size, not an input-size cap.  The old
        // form has one load and one equality per lane/key; the recovered loop
        // has a fixed mask/ctz/clear body and one comparison site per key.
        let old_value_ops = width.saturating_mul(key_count.saturating_add(1));
        let new_value_ops = key_count.saturating_add(8);
        if old_value_ops <= new_value_ops {
            return None;
        }

        Some(RewritePlan {
            block: self.block.id,
            root_index: self.root_index,
            result,
            outer,
            gate: candidate.gate,
            flags,
            address: candidate.comparisons.address,
            element_width: candidate.comparisons.element_width,
            element_type: candidate.comparisons.element_type,
            lane_count: candidate.comparisons.lane_count,
            constants: candidate.comparisons.constants,
        })
    }

    fn match_candidate_term(&self, term: RegisterId, width: usize) -> Option<CandidateTerm> {
        let SIRInstruction::Binary(_, lhs, BinaryOp::And, rhs) = self.definition(term)? else {
            return None;
        };
        let lhs_match = self
            .match_compare_union(*rhs, width)
            .map(|comparisons| (*lhs, comparisons));
        let rhs_match = self
            .match_compare_union(*lhs, width)
            .map(|comparisons| (*rhs, comparisons));
        let (gate, comparisons) = match (lhs_match, rhs_match) {
            (Some(found), None) | (None, Some(found)) => found,
            _ => return None,
        };
        Some(CandidateTerm {
            term,
            gate,
            comparisons,
        })
    }

    fn match_compare_union(&self, root: RegisterId, width: usize) -> Option<CompareUnion> {
        let (leaves, or_defs) = self.flatten_or(root, width)?;
        let mut packs = Vec::with_capacity(leaves.len());
        for leaf in leaves {
            packs.push(self.match_compare_pack(leaf, width)?);
        }
        let first = packs.first()?;
        let address = first.address;
        let element_width = first.element_width;
        let element_type = first.element_type.clone();
        let loads_by_lane = first.loads_by_lane.clone();
        if packs.iter().any(|pack| {
            pack.address != address
                || pack.element_width != element_width
                || pack.element_type != element_type
                || pack.loads_by_lane != loads_by_lane
        }) {
            return None;
        }

        let mut constants = Vec::with_capacity(packs.len());
        let mut values = Vec::with_capacity(packs.len());
        let mut single_use_defs = or_defs;
        for pack in packs {
            if values.contains(&pack.constant_value) {
                return None;
            }
            values.push(pack.constant_value);
            constants.push(pack.constant);
            single_use_defs.push(pack.concat);
            single_use_defs.extend(pack.equality_defs);
        }
        Some(CompareUnion {
            address,
            element_width,
            element_type,
            lane_count: loads_by_lane.len(),
            constants,
            loads_by_lane,
            single_use_defs,
        })
    }

    fn match_compare_pack(&self, concat: RegisterId, width: usize) -> Option<ComparePack> {
        let SIRInstruction::Concat(_, parts) = self.definition(concat)? else {
            return None;
        };
        if parts.len() != width || unsigned_bit_width(self.eu, concat)? != width {
            return None;
        }

        let mut address = None;
        let mut element_width = None;
        let mut loads_by_lane = vec![None; width];
        let mut equality_defs = Vec::with_capacity(width);
        let mut constant = None;
        let mut constant_value = None;

        for (part_index, &equality) in parts.iter().enumerate() {
            if register_width(self.eu, equality)? != 1 {
                return None;
            }
            let SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs) = self.definition(equality)?
            else {
                return None;
            };
            let lhs_match = self.match_static_load_and_constant(*lhs, *rhs);
            let rhs_match = self.match_static_load_and_constant(*rhs, *lhs);
            let (load, load_address, offset, bits, key, value) = match (lhs_match, rhs_match) {
                (Some(found), None) | (None, Some(found)) => found,
                _ => return None,
            };
            let shape = *self.unpacked_arrays.get(&load_address.absolute_addr())?;
            if bits != shape.element_width || width > shape.element_count {
                return None;
            }
            let lane = width - 1 - part_index;
            if offset != lane.checked_mul(bits)? {
                return None;
            }
            if address.is_some_and(|previous| previous != load_address)
                || element_width.is_some_and(|previous| previous != bits)
                || constant_value
                    .as_ref()
                    .is_some_and(|previous| previous != &value)
            {
                return None;
            }
            address = Some(load_address);
            element_width = Some(bits);
            constant.get_or_insert(key);
            constant_value.get_or_insert(value);
            loads_by_lane[lane] = Some(load);
            equality_defs.push(equality);
        }

        let address = address?;
        let element_width = element_width?;
        let loads_by_lane = loads_by_lane.into_iter().collect::<Option<Vec<_>>>()?;
        if !self.same_memory_version_until_root(address, element_width, &loads_by_lane) {
            return None;
        }

        Some(ComparePack {
            concat,
            address,
            element_width,
            element_type: self.eu.register_map.get(&loads_by_lane[0])?.clone(),
            loads_by_lane,
            equality_defs,
            constant: constant?,
            constant_value: constant_value?,
        })
    }

    fn match_static_load_and_constant(
        &self,
        load: RegisterId,
        constant: RegisterId,
    ) -> Option<(
        RegisterId,
        RegionedAbsoluteAddr,
        usize,
        usize,
        RegisterId,
        SIRValue,
    )> {
        let SIRInstruction::Load(_, address, SIROffset::Static(offset), bits) =
            self.definition(load)?
        else {
            return None;
        };
        let SIRInstruction::Imm(_, value) = self.definition(constant)? else {
            return None;
        };
        if !value.mask.to_u64_digits().is_empty()
            || register_width(self.eu, load)? != *bits
            || register_width(self.eu, constant)? != *bits
        {
            return None;
        }
        Some((load, *address, *offset, *bits, constant, value.clone()))
    }

    fn flatten_or(
        &self,
        root: RegisterId,
        width: usize,
    ) -> Option<(Vec<RegisterId>, Vec<RegisterId>)> {
        let mut stack = vec![root];
        let mut seen = HashSet::default();
        let mut leaves = Vec::new();
        let mut definitions = Vec::new();
        while let Some(current) = stack.pop() {
            if !seen.insert(current) || unsigned_bit_width(self.eu, current)? != width {
                return None;
            }
            match self.definition(current) {
                Some(SIRInstruction::Binary(_, lhs, BinaryOp::Or, rhs))
                    if unsigned_bit_width(self.eu, *lhs) == Some(width)
                        && unsigned_bit_width(self.eu, *rhs) == Some(width) =>
                {
                    definitions.push(current);
                    stack.push(*rhs);
                    stack.push(*lhs);
                }
                _ => leaves.push(current),
            }
        }
        Some((leaves, definitions))
    }

    fn same_memory_version_until_root(
        &self,
        address: RegionedAbsoluteAddr,
        element_width: usize,
        loads: &[RegisterId],
    ) -> bool {
        let Some(first_load) = loads
            .iter()
            .filter_map(|load| self.definitions.get(load).copied())
            .min()
        else {
            return false;
        };
        let Some(search_width) = loads.len().checked_mul(element_width) else {
            return false;
        };
        self.block.instructions[first_load + 1..self.root_index]
            .iter()
            .all(|instruction| !writes_range(instruction, address, 0, search_width))
    }

    fn definition(&self, register: RegisterId) -> Option<&SIRInstruction<RegionedAbsoluteAddr>> {
        let &index = self.definitions.get(&register)?;
        (index < self.root_index).then(|| &self.block.instructions[index])
    }
}

fn unsigned_bit_width(eu: &ExecutionUnit<RegionedAbsoluteAddr>, reg: RegisterId) -> Option<usize> {
    match eu.register_map.get(&reg)? {
        RegisterType::Bit {
            width,
            signed: false,
        } => Some(*width),
        RegisterType::Bit { signed: true, .. } | RegisterType::Logic { .. } => None,
    }
}

fn register_width(eu: &ExecutionUnit<RegionedAbsoluteAddr>, reg: RegisterId) -> Option<usize> {
    eu.register_map.get(&reg).map(RegisterType::width)
}

fn writes_range(
    instruction: &SIRInstruction<RegionedAbsoluteAddr>,
    address: RegionedAbsoluteAddr,
    read_offset: usize,
    read_width: usize,
) -> bool {
    let (write_address, offset, width) = match instruction {
        SIRInstruction::Store(write_address, offset, width, ..) => (*write_address, offset, *width),
        SIRInstruction::Commit(_, destination, offset, width, _) => (*destination, offset, *width),
        _ => return false,
    };
    if write_address != address || width == 0 {
        return false;
    }
    let SIROffset::Static(write_offset) = offset else {
        return true;
    };
    let read_end = read_offset.saturating_add(read_width);
    let write_end = write_offset.saturating_add(width);
    *write_offset < read_end && read_offset < write_end
}

fn register_use_counts(eu: &ExecutionUnit<RegionedAbsoluteAddr>) -> HashMap<RegisterId, usize> {
    let mut counts = HashMap::default();
    let mut add = |register: RegisterId| {
        *counts.entry(register).or_insert(0usize) += 1;
    };
    for block in eu.blocks.values() {
        for instruction in &block.instructions {
            match instruction {
                SIRInstruction::Imm(..) => {}
                SIRInstruction::Binary(_, lhs, _, rhs) => {
                    add(*lhs);
                    add(*rhs);
                }
                SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, ..) => {
                    add(*source);
                }
                SIRInstruction::Load(_, _, offset, _) => {
                    for register in offset.dynamic_registers().into_iter().flatten() {
                        add(register);
                    }
                }
                SIRInstruction::Store(_, offset, _, source, _, _) => {
                    add(*source);
                    for register in offset.dynamic_registers().into_iter().flatten() {
                        add(register);
                    }
                }
                SIRInstruction::Commit(_, _, offset, _, _) => {
                    for register in offset.dynamic_registers().into_iter().flatten() {
                        add(register);
                    }
                }
                SIRInstruction::Concat(_, parts)
                | SIRInstruction::RuntimeEvent { args: parts, .. }
                | SIRInstruction::CombCaptureEvent { args: parts, .. } => {
                    for &part in parts {
                        add(part);
                    }
                }
                SIRInstruction::Mux(_, condition, true_value, false_value) => {
                    add(*condition);
                    add(*true_value);
                    add(*false_value);
                }
                SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
                    add(*old);
                    add(*new);
                }
            }
        }
        match &block.terminator {
            SIRTerminator::Jump(_, arguments) => {
                for &argument in arguments {
                    add(argument);
                }
            }
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            } => {
                add(*cond);
                for &argument in true_block.1.iter().chain(&false_block.1) {
                    add(argument);
                }
            }
            SIRTerminator::Switch { selector, .. } => add(*selector),
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    counts
}

fn fresh_register(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    next_register: &mut usize,
    ty: RegisterType,
) -> RegisterId {
    loop {
        *next_register += 1;
        let register = RegisterId(*next_register);
        if let std::collections::hash_map::Entry::Vacant(entry) = eu.register_map.entry(register) {
            entry.insert(ty);
            return register;
        }
    }
}

fn fresh_block(next_block: &mut usize) -> BlockId {
    let block = BlockId(*next_block);
    *next_block += 1;
    block
}

fn unsigned_bit_type(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: false,
    }
}

fn apply_plan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: RewritePlan,
    next_block: &mut usize,
    next_register: &mut usize,
) {
    let continuation_id = fresh_block(next_block);
    let true_id = fresh_block(next_block);
    let false_id = fresh_block(next_block);
    let setup_id = fresh_block(next_block);
    let loop_id = fresh_block(next_block);
    let clear_id = fresh_block(next_block);
    let compare_ids = (0..plan.constants.len())
        .map(|_| fresh_block(next_block))
        .collect::<Vec<_>>();

    let result_type = eu.register_map[&plan.result].clone();
    let mask_type = unsigned_bit_type(plan.lane_count);
    let bool_type = unsigned_bit_type(1);
    let index_type = unsigned_bit_type(UnaryOp::CountTrailingZeros.result_width(plan.lane_count));
    let element_type = plan.element_type.clone();

    let initial_mask = fresh_register(eu, next_register, mask_type.clone());
    let remaining = fresh_register(eu, next_register, mask_type.clone());
    let remaining_nonzero = fresh_register(eu, next_register, bool_type.clone());
    let lane_index = fresh_register(eu, next_register, index_type);
    let lane_value = fresh_register(eu, next_register, element_type);
    let equality_results = plan
        .constants
        .iter()
        .map(|_| fresh_register(eu, next_register, bool_type.clone()))
        .collect::<Vec<_>>();
    let mask_one = fresh_register(eu, next_register, mask_type.clone());
    let decremented = fresh_register(eu, next_register, mask_type.clone());
    let cleared = fresh_register(eu, next_register, mask_type.clone());
    let true_value = fresh_register(eu, next_register, result_type.clone());
    let false_value = fresh_register(eu, next_register, result_type);

    let mut block = eu
        .blocks
        .remove(&plan.block)
        .expect("masked-any source block exists");
    let mut suffix = block.instructions.split_off(plan.root_index);
    let removed_root = suffix.remove(0);
    debug_assert!(matches!(
        removed_root,
        SIRInstruction::Unary(dst, UnaryOp::Or, _) if dst == plan.result
    ));
    let old_terminator = std::mem::replace(&mut block.terminator, SIRTerminator::Return);

    let mut entry_instructions = Vec::new();
    if !plan.flags.is_empty() {
        let mut flags = plan.flags[0];
        for &next in &plan.flags[1..] {
            let combined = fresh_register(eu, next_register, mask_type.clone());
            entry_instructions.push(SIRInstruction::Binary(combined, flags, BinaryOp::Or, next));
            flags = combined;
        }
        let active_flags = fresh_register(eu, next_register, mask_type.clone());
        let has_active_flag = fresh_register(eu, next_register, bool_type);
        entry_instructions.push(SIRInstruction::Binary(
            active_flags,
            plan.outer,
            BinaryOp::And,
            flags,
        ));
        entry_instructions.push(SIRInstruction::Unary(
            has_active_flag,
            UnaryOp::Or,
            active_flags,
        ));
        block.instructions.extend(entry_instructions);
        block.terminator = SIRTerminator::Branch {
            cond: has_active_flag,
            true_block: (true_id, Vec::new()),
            false_block: (setup_id, Vec::new()),
        };
    } else {
        block.terminator = SIRTerminator::Jump(setup_id, Vec::new());
    }

    let setup = BasicBlock {
        id: setup_id,
        params: Vec::new(),
        instructions: vec![SIRInstruction::Binary(
            initial_mask,
            plan.outer,
            BinaryOp::And,
            plan.gate,
        )],
        terminator: SIRTerminator::Jump(loop_id, vec![initial_mask]),
    };
    let loop_block = BasicBlock {
        id: loop_id,
        params: vec![remaining],
        instructions: vec![SIRInstruction::Unary(
            remaining_nonzero,
            UnaryOp::Or,
            remaining,
        )],
        terminator: SIRTerminator::Branch {
            cond: remaining_nonzero,
            true_block: (compare_ids[0], Vec::new()),
            false_block: (false_id, Vec::new()),
        },
    };

    let mut compare_blocks = Vec::with_capacity(compare_ids.len());
    for (index, ((&block_id, &constant), &equality)) in compare_ids
        .iter()
        .zip(&plan.constants)
        .zip(&equality_results)
        .enumerate()
    {
        let mut instructions = Vec::new();
        if index == 0 {
            instructions.push(SIRInstruction::Unary(
                lane_index,
                UnaryOp::CountTrailingZeros,
                remaining,
            ));
            instructions.push(SIRInstruction::Load(
                lane_value,
                plan.address,
                SIROffset::Element {
                    index: lane_index,
                    element_width: plan.element_width,
                    bit_offset: 0,
                    dynamic_bit_offset: None,
                },
                plan.element_width,
            ));
        }
        instructions.push(SIRInstruction::Binary(
            equality,
            lane_value,
            BinaryOp::Eq,
            constant,
        ));
        let false_target = compare_ids.get(index + 1).copied().unwrap_or(clear_id);
        compare_blocks.push(BasicBlock {
            id: block_id,
            params: Vec::new(),
            instructions,
            terminator: SIRTerminator::Branch {
                cond: equality,
                true_block: (true_id, Vec::new()),
                false_block: (false_target, Vec::new()),
            },
        });
    }

    let clear = BasicBlock {
        id: clear_id,
        params: Vec::new(),
        instructions: vec![
            SIRInstruction::Imm(mask_one, SIRValue::new(1u8)),
            SIRInstruction::Binary(decremented, remaining, BinaryOp::Sub, mask_one),
            SIRInstruction::Binary(cleared, remaining, BinaryOp::And, decremented),
        ],
        terminator: SIRTerminator::Jump(loop_id, vec![cleared]),
    };
    let true_block = BasicBlock {
        id: true_id,
        params: Vec::new(),
        instructions: vec![SIRInstruction::Imm(true_value, SIRValue::new(1u8))],
        terminator: SIRTerminator::Jump(continuation_id, vec![true_value]),
    };
    let false_block = BasicBlock {
        id: false_id,
        params: Vec::new(),
        instructions: vec![SIRInstruction::Imm(false_value, SIRValue::new(0u8))],
        terminator: SIRTerminator::Jump(continuation_id, vec![false_value]),
    };
    let continuation = BasicBlock {
        id: continuation_id,
        params: vec![plan.result],
        instructions: suffix,
        terminator: old_terminator,
    };

    eu.blocks.insert(plan.block, block);
    eu.blocks.insert(setup_id, setup);
    eu.blocks.insert(loop_id, loop_block);
    eu.blocks.insert(clear_id, clear);
    eu.blocks.insert(true_id, true_block);
    eu.blocks.insert(false_id, false_block);
    eu.blocks.insert(continuation_id, continuation);
    for compare_block in compare_blocks {
        eu.blocks.insert(compare_block.id, compare_block);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use celox_design::StateObjectId as VarId;
    use num_traits::ToPrimitive;

    const LANES: usize = 8;
    const ELEMENT_WIDTH: usize = 12;
    const KEYS: [u64; 3] = [0x100, 0x300, 0x180];

    fn address(raw: u32) -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: InstanceId(0),
            var_id: VarId::from_raw(raw),
        }
    }

    fn array_address() -> RegionedAbsoluteAddr {
        address(0)
    }

    fn output_address() -> RegionedAbsoluteAddr {
        address(1)
    }

    struct Builder {
        next_register: usize,
        register_map: HashMap<RegisterId, RegisterType>,
        instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    }

    impl Builder {
        fn new() -> Self {
            Self {
                next_register: 0,
                register_map: HashMap::default(),
                instructions: Vec::new(),
            }
        }

        fn bit(&mut self, width: usize) -> RegisterId {
            let register = RegisterId(self.next_register);
            self.next_register += 1;
            self.register_map.insert(register, unsigned_bit_type(width));
            register
        }

        fn logic(&mut self, width: usize) -> RegisterId {
            let register = RegisterId(self.next_register);
            self.next_register += 1;
            self.register_map
                .insert(register, RegisterType::Logic { width });
            register
        }

        fn imm(&mut self, width: usize, value: u64) -> RegisterId {
            let register = self.bit(width);
            self.instructions
                .push(SIRInstruction::Imm(register, SIRValue::new(value)));
            register
        }

        fn binary(
            &mut self,
            width: usize,
            lhs: RegisterId,
            op: BinaryOp,
            rhs: RegisterId,
        ) -> RegisterId {
            let register = self.bit(width);
            self.instructions
                .push(SIRInstruction::Binary(register, lhs, op, rhs));
            register
        }

        fn logic_binary(
            &mut self,
            width: usize,
            lhs: RegisterId,
            op: BinaryOp,
            rhs: RegisterId,
        ) -> RegisterId {
            let register = self.logic(width);
            self.instructions
                .push(SIRInstruction::Binary(register, lhs, op, rhs));
            register
        }
    }

    struct Fixture {
        eu: ExecutionUnit<RegionedAbsoluteAddr>,
        outer: RegisterId,
        gate: RegisterId,
        flag_a: RegisterId,
        flag_b: RegisterId,
    }

    fn fixture(reordered_lanes: bool, writes_between: bool, writes_after: bool) -> Fixture {
        let mut builder = Builder::new();
        let outer = builder.bit(LANES);
        let gate = builder.bit(LANES);
        let flag_a = builder.bit(LANES);
        let flag_b = builder.bit(LANES);
        let params = vec![outer, gate, flag_a, flag_b];

        let mut loads = Vec::new();
        for lane in 0..LANES {
            let load = builder.logic(ELEMENT_WIDTH);
            let physical_lane = if reordered_lanes {
                match lane {
                    0 => 1,
                    1 => 0,
                    _ => lane,
                }
            } else {
                lane
            };
            builder.instructions.push(SIRInstruction::Load(
                load,
                array_address(),
                SIROffset::Static(physical_lane * ELEMENT_WIDTH),
                ELEMENT_WIDTH,
            ));
            loads.push(load);
        }
        if writes_between {
            let write_value = builder.imm(ELEMENT_WIDTH, 0x55);
            builder.instructions.push(SIRInstruction::Store(
                array_address(),
                SIROffset::Static(0),
                ELEMENT_WIDTH,
                write_value,
                Vec::new(),
                Vec::new(),
            ));
        }

        let mut packs = Vec::new();
        for key in KEYS {
            let key_register = builder.imm(ELEMENT_WIDTH, key);
            let mut equalities = Vec::new();
            for &load in &loads {
                equalities.push(builder.logic_binary(1, load, BinaryOp::Eq, key_register));
            }
            let pack = builder.bit(LANES);
            builder.instructions.push(SIRInstruction::Concat(
                pack,
                equalities.into_iter().rev().collect(),
            ));
            packs.push(pack);
        }

        let compare_01 = builder.binary(LANES, packs[0], BinaryOp::Or, packs[1]);
        let compare_all = builder.binary(LANES, compare_01, BinaryOp::Or, packs[2]);
        let gated = builder.binary(LANES, gate, BinaryOp::And, compare_all);
        let flags = builder.binary(LANES, flag_a, BinaryOp::Or, flag_b);
        let body = builder.binary(LANES, gated, BinaryOp::Or, flags);
        let active = builder.binary(LANES, outer, BinaryOp::And, body);
        let result = builder.logic(1);
        builder
            .instructions
            .push(SIRInstruction::Unary(result, UnaryOp::Or, active));
        builder.instructions.push(SIRInstruction::Store(
            output_address(),
            SIROffset::Static(0),
            1,
            result,
            Vec::new(),
            Vec::new(),
        ));
        if writes_after {
            let write_value = builder.imm(ELEMENT_WIDTH, 0x55);
            builder.instructions.push(SIRInstruction::Store(
                array_address(),
                SIROffset::Static(0),
                ELEMENT_WIDTH,
                write_value,
                Vec::new(),
                Vec::new(),
            ));
        }

        let block = BasicBlock {
            id: BlockId(0),
            params,
            instructions: builder.instructions,
            terminator: SIRTerminator::Return,
        };
        Fixture {
            eu: ExecutionUnit {
                entry_block_id: BlockId(0),
                blocks: [(BlockId(0), block)].into_iter().collect(),
                register_map: builder.register_map,
            },
            outer,
            gate,
            flag_a,
            flag_b,
        }
    }

    fn pass() -> MaskedArrayAnyPass {
        MaskedArrayAnyPass {
            unpacked_arrays: [(
                array_address().absolute_addr(),
                ArrayShape {
                    element_width: ELEMENT_WIDTH,
                    element_count: LANES,
                },
            )]
            .into_iter()
            .collect(),
        }
    }

    fn truncate(value: u64, width: usize) -> u64 {
        if width >= 64 {
            value
        } else {
            value & ((1u64 << width) - 1)
        }
    }

    fn execute(
        fixture: &Fixture,
        outer: u64,
        gate: u64,
        flag_a: u64,
        flag_b: u64,
        lanes: [u64; LANES],
    ) -> (u64, usize) {
        let eu = &fixture.eu;
        let mut registers = HashMap::default();
        registers.insert(fixture.outer, outer);
        registers.insert(fixture.gate, gate);
        registers.insert(fixture.flag_a, flag_a);
        registers.insert(fixture.flag_b, flag_b);
        let mut memory = HashMap::default();
        for (lane, value) in lanes.into_iter().enumerate() {
            memory.insert(
                (array_address(), lane * ELEMENT_WIDTH),
                truncate(value, ELEMENT_WIDTH),
            );
        }
        let mut current = eu.entry_block_id;
        let mut array_loads = 0usize;
        for _ in 0..1024 {
            let block = &eu.blocks[&current];
            for instruction in &block.instructions {
                match instruction {
                    SIRInstruction::Imm(destination, value) => {
                        let value = value.payload.to_u64().unwrap_or(0);
                        registers.insert(
                            *destination,
                            truncate(value, eu.register_map[destination].width()),
                        );
                    }
                    SIRInstruction::Binary(destination, lhs, op, rhs) => {
                        let lhs = registers[lhs];
                        let rhs = registers[rhs];
                        let value = match op {
                            BinaryOp::And => lhs & rhs,
                            BinaryOp::Or => lhs | rhs,
                            BinaryOp::Sub => lhs.wrapping_sub(rhs),
                            BinaryOp::Eq => u64::from(lhs == rhs),
                            other => panic!("unsupported test binary operation {other:?}"),
                        };
                        registers.insert(
                            *destination,
                            truncate(value, eu.register_map[destination].width()),
                        );
                    }
                    SIRInstruction::Unary(destination, op, source) => {
                        let source = registers[source];
                        let value = match op {
                            UnaryOp::Or => u64::from(source != 0),
                            UnaryOp::CountTrailingZeros => source.trailing_zeros() as u64,
                            other => panic!("unsupported test unary operation {other:?}"),
                        };
                        registers.insert(
                            *destination,
                            truncate(value, eu.register_map[destination].width()),
                        );
                    }
                    SIRInstruction::Load(destination, address, offset, _) => {
                        if *address == array_address() {
                            array_loads += 1;
                        }
                        let offset = match offset {
                            SIROffset::Static(offset)
                            | SIROffset::PackedElements {
                                bit_offset: offset, ..
                            } => *offset,
                            SIROffset::Dynamic(register) => registers[register] as usize,
                            SIROffset::Element {
                                index,
                                element_width,
                                bit_offset,
                                dynamic_bit_offset,
                            } => {
                                registers[index] as usize * element_width
                                    + bit_offset
                                    + dynamic_bit_offset
                                        .map(|register| registers[&register] as usize)
                                        .unwrap_or(0)
                            }
                        };
                        registers.insert(
                            *destination,
                            memory.get(&(*address, offset)).copied().unwrap_or(0),
                        );
                    }
                    SIRInstruction::Store(address, SIROffset::Static(offset), _, source, _, _) => {
                        memory.insert((*address, *offset), registers[source]);
                    }
                    SIRInstruction::Concat(destination, parts) => {
                        let mut value = 0u64;
                        for part in parts {
                            let width = eu.register_map[part].width();
                            value = (value << width) | registers[part];
                        }
                        registers.insert(
                            *destination,
                            truncate(value, eu.register_map[destination].width()),
                        );
                    }
                    other => panic!("unsupported test instruction {other:?}"),
                }
            }

            let (target, arguments) = match &block.terminator {
                SIRTerminator::Jump(target, arguments) => (*target, arguments.clone()),
                SIRTerminator::Branch {
                    cond,
                    true_block,
                    false_block,
                } => {
                    if registers[cond] != 0 {
                        (true_block.0, true_block.1.clone())
                    } else {
                        (false_block.0, false_block.1.clone())
                    }
                }
                SIRTerminator::Switch { .. } => {
                    panic!("unexpected Switch in masked-array test")
                }
                SIRTerminator::Return => {
                    return (
                        memory.get(&(output_address(), 0)).copied().unwrap_or(0),
                        array_loads,
                    );
                }
                SIRTerminator::Error(code) => panic!("unexpected test error {code}"),
            };
            let values = arguments
                .iter()
                .map(|register| registers[register])
                .collect::<Vec<_>>();
            assert_eq!(eu.blocks[&target].params.len(), values.len());
            for (&parameter, value) in eu.blocks[&target].params.iter().zip(values) {
                registers.insert(parameter, value);
            }
            current = target;
        }
        panic!("test SIR did not terminate");
    }

    fn assert_unchanged(
        before: &ExecutionUnit<RegionedAbsoluteAddr>,
        after: &ExecutionUnit<RegionedAbsoluteAddr>,
    ) {
        assert_eq!(after.entry_block_id, before.entry_block_id);
        assert_eq!(after.register_map, before.register_map);
        assert_eq!(after.blocks, before.blocks);
    }

    #[test]
    fn recovers_active_lane_search_and_preserves_results() {
        let mut fixture = fixture(false, false, false);
        fixture.eu.verify_result().unwrap();
        let before = Fixture {
            eu: fixture.eu.clone(),
            outer: fixture.outer,
            gate: fixture.gate,
            flag_a: fixture.flag_a,
            flag_b: fixture.flag_b,
        };

        pass().run(&mut fixture.eu, &PassOptions::default());
        fixture.eu.verify_result().unwrap();

        let static_array_loads = fixture
            .eu
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Load(_, address, SIROffset::Static(_), _)
                        if *address == array_address()
                )
            })
            .count();
        let dynamic_array_loads = fixture
            .eu
            .blocks
            .values()
            .flat_map(|block| &block.instructions)
            .filter(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Load(_, address, SIROffset::Element { .. }, _)
                        if *address == array_address()
                )
            })
            .count();
        assert_eq!(static_array_loads, 0);
        assert_eq!(dynamic_array_loads, 1);

        let cases = [
            (0b0000_0100, 0, 0b0000_0100, 0, [0; LANES], true),
            (0, 0, 0b0000_0100, 0, [0; LANES], false),
            (
                0b0000_0001,
                0b0000_0001,
                0,
                0,
                [KEYS[0], 0, 0, 0, 0, 0, 0, 0],
                true,
            ),
            (
                0b0001_0000,
                0b0001_0000,
                0,
                0,
                [0, 0, 0, 0, KEYS[1], 0, 0, 0],
                true,
            ),
            (
                0b1000_0000,
                0b1000_0000,
                0,
                0,
                [0, 0, 0, 0, 0, 0, 0, KEYS[2]],
                true,
            ),
            (
                0b1010_0101,
                0b1010_0101,
                0,
                0,
                [1, 2, 3, 4, 5, 6, 7, 8],
                false,
            ),
            (
                0b1010_0101,
                0b1010_0101,
                0,
                0,
                [1, 2, KEYS[2], 4, 5, 6, 7, 8],
                true,
            ),
            (
                0b0000_0001,
                0b0000_0010,
                0,
                0,
                [0, KEYS[0], 0, 0, 0, 0, 0, 0],
                false,
            ),
        ];
        for (outer, gate, flag_a, flag_b, lanes, expected) in cases {
            let before_result = execute(&before, outer, gate, flag_a, flag_b, lanes);
            let after_result = execute(&fixture, outer, gate, flag_a, flag_b, lanes);
            assert_eq!(before_result.0, u64::from(expected));
            assert_eq!(after_result.0, before_result.0);
        }

        let (_, flag_loads) = execute(&fixture, 0b0000_0100, 0, 0b0000_0100, 0, [0; LANES]);
        let (_, empty_loads) = execute(&fixture, 0, u64::MAX, 0, 0, [KEYS[0]; LANES]);
        let (_, two_candidate_loads) = execute(
            &fixture,
            0b0000_0101,
            0b0000_0101,
            0,
            0,
            [1, 0, KEYS[1], 0, 0, 0, 0, 0],
        );
        assert_eq!(flag_loads, 0);
        assert_eq!(empty_loads, 0);
        assert_eq!(two_candidate_loads, 2);
    }

    #[test]
    fn leaves_four_state_mode_unchanged() {
        let mut fixture = fixture(false, false, false);
        let before = fixture.eu.clone();
        let options = PassOptions {
            four_state: true,
            ..PassOptions::default()
        };
        pass().run(&mut fixture.eu, &options);
        assert_unchanged(&before, &fixture.eu);
    }

    #[test]
    fn requires_declared_unpacked_array_shape() {
        let mut fixture = fixture(false, false, false);
        let before = fixture.eu.clone();
        MaskedArrayAnyPass::default().run(&mut fixture.eu, &PassOptions::default());
        assert_unchanged(&before, &fixture.eu);
    }

    #[test]
    fn rejects_reordered_lane_mapping() {
        let mut fixture = fixture(true, false, false);
        let before = fixture.eu.clone();
        pass().run(&mut fixture.eu, &PassOptions::default());
        assert_unchanged(&before, &fixture.eu);
    }

    #[test]
    fn preserves_load_memory_version_when_array_is_written() {
        let mut fixture = fixture(false, true, false);
        let before = fixture.eu.clone();
        pass().run(&mut fixture.eu, &PassOptions::default());
        assert_unchanged(&before, &fixture.eu);
    }

    #[test]
    fn write_after_reduction_does_not_constrain_load_placement() {
        let mut fixture = fixture(false, false, true);
        fixture.eu.verify_result().unwrap();
        pass().run(&mut fixture.eu, &PassOptions::default());
        fixture.eu.verify_result().unwrap();
        assert!(fixture.eu.blocks.len() > 1);
        assert!(fixture.eu.blocks.values().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction,
                    SIRInstruction::Load(_, address, SIROffset::Element { .. }, _)
                        if *address == array_address()
                )
            })
        }));
    }
}
