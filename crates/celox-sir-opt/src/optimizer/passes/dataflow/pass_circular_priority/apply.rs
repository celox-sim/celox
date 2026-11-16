//! Emitting packed expressions and applying the recognized plans.

use super::*;

/// Find loop definitions used elsewhere with one whole-EU use walk, then keep
/// only plans whose escaping values are pure loop invariants.  This is linear
/// in the EU plus the dependency closure of the values that must be hoisted;
/// it does not rescan the complete EU once per natural loop.
pub(super) fn prepare_escaping_definitions(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    definitions: &HashMap<RegisterId, Definition>,
    plans: &mut Vec<CircularPriorityPlan>,
) {
    let mut block_owner = HashMap::<BlockId, usize>::default();
    let mut register_owner = HashMap::<RegisterId, usize>::default();
    for (plan_index, plan) in plans.iter().enumerate() {
        for &block_id in &plan.loop_blocks {
            block_owner.insert(block_id, plan_index);
            let block = &eu.blocks[&block_id];
            for &parameter in &block.params {
                register_owner.insert(parameter, plan_index);
            }
            for instruction in &block.instructions {
                if let Some(register) = def_reg(instruction) {
                    register_owner.insert(register, plan_index);
                }
            }
        }
    }

    let mut escaping_roots = vec![HashSet::default(); plans.len()];
    let mut uses = Vec::new();
    for (&block_id, block) in &eu.blocks {
        for instruction in &block.instructions {
            uses.clear();
            instruction_uses(instruction, &mut uses);
            for &register in &uses {
                let Some(&owner) = register_owner.get(&register) else {
                    continue;
                };
                if block_owner.get(&block_id) != Some(&owner) {
                    escaping_roots[owner].insert(register);
                }
            }
        }
        uses.clear();
        terminator_uses(&block.terminator, &mut uses);
        for &register in &uses {
            let Some(&owner) = register_owner.get(&register) else {
                continue;
            };
            if block_owner.get(&block_id) != Some(&owner) {
                escaping_roots[owner].insert(register);
            }
        }
    }

    let mut keep = vec![true; plans.len()];
    for (plan_index, plan) in plans.iter_mut().enumerate() {
        let loop_blocks = plan.loop_blocks.iter().copied().collect::<HashSet<_>>();
        let Some(instructions) = hoist_escaping_definitions(
            eu,
            cfg,
            plan.preheader,
            &loop_blocks,
            definitions,
            &escaping_roots[plan_index],
        ) else {
            keep[plan_index] = false;
            continue;
        };
        plan.hoisted_instructions = instructions;
    }

    let old_plans = std::mem::take(plans);
    *plans = old_plans
        .into_iter()
        .zip(keep)
        .filter_map(|(plan, keep)| keep.then_some(plan))
        .collect();
}

pub(super) fn hoist_escaping_definitions(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    preheader: BlockId,
    loop_blocks: &HashSet<BlockId>,
    definitions: &HashMap<RegisterId, Definition>,
    roots: &HashSet<RegisterId>,
) -> Option<Vec<SIRInstruction<RegionedAbsoluteAddr>>> {
    let mut roots = roots.iter().copied().collect::<Vec<_>>();
    roots.sort_unstable();
    let mut state = HashMap::<RegisterId, bool>::default();
    let mut hoisted = Vec::new();

    for root in roots {
        let mut stack = vec![(root, false)];
        while let Some((register, expanded)) = stack.pop() {
            if expanded {
                if state.get(&register) == Some(&true) {
                    continue;
                }
                let definition = instruction(eu, definitions, register)?.clone();
                hoisted.push(definition);
                state.insert(register, true);
                continue;
            }
            match state.get(&register) {
                Some(true) => continue,
                Some(false) => return None,
                None => {}
            }
            let Definition::Instruction { block, .. } = *definitions.get(&register)? else {
                return None;
            };
            if !loop_blocks.contains(&block) {
                return None;
            }
            let definition = instruction(eu, definitions, register)?;
            if !matches!(
                definition,
                SIRInstruction::Imm(..)
                    | SIRInstruction::Binary(..)
                    | SIRInstruction::Unary(..)
                    | SIRInstruction::Slice(..)
                    | SIRInstruction::Concat(..)
                    | SIRInstruction::Mux(..)
            ) {
                return None;
            }

            state.insert(register, false);
            stack.push((register, true));
            let mut operands = Vec::new();
            instruction_uses(definition, &mut operands);
            for operand in operands.into_iter().rev() {
                let operand_definition = *definitions.get(&operand)?;
                if loop_blocks.contains(&operand_definition.block()) {
                    stack.push((operand, false));
                } else if !cfg.dominates(operand_definition.block(), preheader) {
                    return None;
                }
            }
        }
    }
    Some(hoisted)
}

pub(super) fn instruction_uses(
    instruction: &SIRInstruction<RegionedAbsoluteAddr>,
    uses: &mut Vec<RegisterId>,
) {
    instruction.for_each_use(|register| uses.push(register));
}

pub(super) fn terminator_uses(terminator: &SIRTerminator, uses: &mut Vec<RegisterId>) {
    match terminator {
        SIRTerminator::Jump(_, arguments) => uses.extend(arguments.iter().copied()),
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            uses.push(*cond);
            uses.extend(true_block.1.iter().copied());
            uses.extend(false_block.1.iter().copied());
        }
        SIRTerminator::Switch { selector, .. } => uses.push(*selector),
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

#[derive(Clone, Copy)]
pub(super) enum EmittedValue {
    Constant(bool),
    Register(RegisterId),
}

pub(super) struct ExpressionEmitter<'a> {
    pub(super) eu: &'a mut ExecutionUnit<RegionedAbsoluteAddr>,
    pub(super) next_register: &'a mut usize,
    pub(super) width: usize,
    pub(super) instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    pub(super) zero: Option<RegisterId>,
    pub(super) ones: Option<RegisterId>,
}

impl ExpressionEmitter<'_> {
    pub(super) fn register(&mut self, value: EmittedValue) -> RegisterId {
        match value {
            EmittedValue::Register(register) => register,
            EmittedValue::Constant(false) => {
                if let Some(register) = self.zero {
                    return register;
                }
                let register =
                    fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                self.instructions
                    .push(SIRInstruction::Imm(register, SIRValue::new(0u8)));
                self.zero = Some(register);
                register
            }
            EmittedValue::Constant(true) => {
                if let Some(register) = self.ones {
                    return register;
                }
                let mask = (BigUint::from(1u8) << self.width) - BigUint::from(1u8);
                let register =
                    fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                self.instructions
                    .push(SIRInstruction::Imm(register, SIRValue::new(mask)));
                self.ones = Some(register);
                register
            }
        }
    }

    pub(super) fn emit_lane_eq(
        &mut self,
        address: RegionedAbsoluteAddr,
        element_width: usize,
        bit_offset: usize,
        field_width: usize,
        value: RegisterId,
    ) -> RegisterId {
        let mut predicates = Vec::with_capacity(self.width);
        for lane in (0..self.width).rev() {
            let loaded = fresh_register(self.eu, self.next_register, unsigned_type(field_width));
            self.instructions.push(SIRInstruction::Load(
                loaded,
                address,
                SIROffset::Static(lane * element_width + bit_offset),
                field_width,
            ));
            let predicate = fresh_register(self.eu, self.next_register, unsigned_type(1));
            self.instructions.push(SIRInstruction::Binary(
                predicate,
                loaded,
                BinaryOp::Eq,
                value,
            ));
            predicates.push(predicate);
        }
        let compact = fresh_register(self.eu, self.next_register, unsigned_type(self.width));
        self.instructions
            .push(SIRInstruction::Concat(compact, predicates));
        compact
    }

    pub(super) fn emit(
        mut self,
        expression: &PackedExpression,
    ) -> (RegisterId, Vec<SIRInstruction<RegionedAbsoluteAddr>>) {
        let mut values = Vec::<EmittedValue>::with_capacity(expression.nodes.len());
        for node in &expression.nodes {
            let value = match *node {
                PackedNode::Zero => EmittedValue::Constant(false),
                PackedNode::Ones => EmittedValue::Constant(true),
                PackedNode::Load {
                    address,
                    unpacked_element_width,
                } => {
                    let register =
                        fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                    self.instructions.push(SIRInstruction::Load(
                        register,
                        address,
                        unpacked_element_width.map_or(SIROffset::Static(0), |element_width| {
                            SIROffset::PackedElements {
                                bit_offset: 0,
                                element_width,
                            }
                        }),
                        self.width,
                    ));
                    EmittedValue::Register(register)
                }
                PackedNode::Broadcast(condition) => {
                    let zero = self.register(EmittedValue::Constant(false));
                    let ones = self.register(EmittedValue::Constant(true));
                    let register =
                        fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                    self.instructions
                        .push(SIRInstruction::Mux(register, condition, ones, zero));
                    EmittedValue::Register(register)
                }
                PackedNode::Prefix(bound) => {
                    let one =
                        fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                    self.instructions
                        .push(SIRInstruction::Imm(one, SIRValue::new(1u8)));
                    let shifted =
                        fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                    self.instructions.push(SIRInstruction::Binary(
                        shifted,
                        one,
                        BinaryOp::Shl,
                        bound,
                    ));
                    let prefix =
                        fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                    self.instructions.push(SIRInstruction::Binary(
                        prefix,
                        shifted,
                        BinaryOp::Sub,
                        one,
                    ));
                    EmittedValue::Register(prefix)
                }
                PackedNode::LaneEq {
                    address,
                    element_width,
                    bit_offset,
                    field_width,
                    value,
                } => EmittedValue::Register(self.emit_lane_eq(
                    address,
                    element_width,
                    bit_offset,
                    field_width,
                    value,
                )),
                PackedNode::Not(source) => match values[source] {
                    EmittedValue::Constant(value) => EmittedValue::Constant(!value),
                    source => {
                        let source = self.register(source);
                        let register =
                            fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                        self.instructions.push(SIRInstruction::Unary(
                            register,
                            UnaryOp::BitNot,
                            source,
                        ));
                        EmittedValue::Register(register)
                    }
                },
                PackedNode::And(lhs, rhs) => match (values[lhs], values[rhs]) {
                    (EmittedValue::Constant(false), _) | (_, EmittedValue::Constant(false)) => {
                        EmittedValue::Constant(false)
                    }
                    (EmittedValue::Constant(true), value)
                    | (value, EmittedValue::Constant(true)) => value,
                    (lhs, rhs) => {
                        let lhs = self.register(lhs);
                        let rhs = self.register(rhs);
                        let register =
                            fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                        self.instructions.push(SIRInstruction::Binary(
                            register,
                            lhs,
                            BinaryOp::And,
                            rhs,
                        ));
                        EmittedValue::Register(register)
                    }
                },
                PackedNode::Or(lhs, rhs) => match (values[lhs], values[rhs]) {
                    (EmittedValue::Constant(true), _) | (_, EmittedValue::Constant(true)) => {
                        EmittedValue::Constant(true)
                    }
                    (EmittedValue::Constant(false), value)
                    | (value, EmittedValue::Constant(false)) => value,
                    (lhs, rhs) => {
                        let lhs = self.register(lhs);
                        let rhs = self.register(rhs);
                        let register =
                            fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                        self.instructions.push(SIRInstruction::Binary(
                            register,
                            lhs,
                            BinaryOp::Or,
                            rhs,
                        ));
                        EmittedValue::Register(register)
                    }
                },
            };
            values.push(value);
        }
        let mut root = values[expression.root];
        if expression.invert {
            root = match root {
                EmittedValue::Constant(value) => EmittedValue::Constant(!value),
                source => {
                    let source = self.register(source);
                    let register =
                        fresh_register(self.eu, self.next_register, unsigned_type(self.width));
                    self.instructions.push(SIRInstruction::Unary(
                        register,
                        UnaryOp::BitNot,
                        source,
                    ));
                    EmittedValue::Register(register)
                }
            };
        }
        let root = self.register(root);
        (root, self.instructions)
    }
}

pub(super) fn apply_plan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: CircularPriorityPlan,
    next_register: &mut usize,
    next_block: &mut usize,
) {
    let selected_block = fresh_block(next_block);
    let empty_block = fresh_block(next_block);
    let emitter = ExpressionEmitter {
        eu,
        next_register,
        width: plan.lanes,
        instructions: Vec::new(),
        zero: None,
        ones: None,
    };
    let (mask, predicate_instructions) = emitter.emit(&plan.predicate);
    let mut instructions = plan.hoisted_instructions;
    instructions.extend(predicate_instructions);

    let doubled_width = plan.lanes * 2;
    let doubled = fresh_register(eu, next_register, unsigned_type(doubled_width));
    let shifted = fresh_register(eu, next_register, unsigned_type(doubled_width));
    let rotated = fresh_register(eu, next_register, unsigned_type(plan.lanes));
    let nonempty = fresh_register(eu, next_register, unsigned_type(1));
    instructions.push(SIRInstruction::Concat(doubled, vec![mask, mask]));
    instructions.push(SIRInstruction::Binary(
        shifted,
        doubled,
        BinaryOp::Shr,
        plan.head,
    ));
    instructions.push(SIRInstruction::Slice(rotated, shifted, 0, plan.lanes));
    instructions.push(SIRInstruction::Unary(nonempty, UnaryOp::Or, rotated));

    let preheader = eu.blocks.get_mut(&plan.preheader).unwrap();
    preheader.instructions.extend(instructions);
    preheader.terminator = SIRTerminator::Branch {
        cond: nonempty,
        true_block: (selected_block, Vec::new()),
        false_block: (empty_block, Vec::new()),
    };

    let count_width = UnaryOp::CountTrailingZeros.result_width(plan.lanes);
    let count = fresh_register(eu, next_register, unsigned_type(count_width));
    let selected_age = fresh_register(eu, next_register, plan.best_type.clone());
    let found = fresh_register(eu, next_register, plan.found_type.clone());
    let mut selected_arguments = vec![selected_age; 2];
    selected_arguments[plan.exit_found_position] = found;
    selected_arguments[plan.exit_best_position] = selected_age;
    let selected = BasicBlock {
        id: selected_block,
        params: Vec::new(),
        instructions: vec![
            SIRInstruction::Unary(count, UnaryOp::CountTrailingZeros, rotated),
            SIRInstruction::Slice(selected_age, count, 0, plan.best_type.width()),
            SIRInstruction::Imm(found, SIRValue::new(1u8)),
        ],
        terminator: SIRTerminator::Jump(plan.exit, selected_arguments),
    };

    let not_found = fresh_register(eu, next_register, plan.found_type);
    let no_age = fresh_register(eu, next_register, plan.best_type);
    let mut empty_arguments = vec![no_age; 2];
    empty_arguments[plan.exit_found_position] = not_found;
    empty_arguments[plan.exit_best_position] = no_age;
    let empty = BasicBlock {
        id: empty_block,
        params: Vec::new(),
        instructions: vec![
            SIRInstruction::Imm(not_found, SIRValue::new(0u8)),
            SIRInstruction::Imm(no_age, SIRValue::new(0u8)),
        ],
        terminator: SIRTerminator::Jump(plan.exit, empty_arguments),
    };

    for block in plan.loop_blocks {
        eu.blocks.remove(&block);
    }
    eu.blocks.insert(selected_block, selected);
    eu.blocks.insert(empty_block, empty);
}

pub(super) fn apply_linear_bitmap_scan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: LinearBitmapScanPlan,
    next_register: &mut usize,
) {
    let emitter = ExpressionEmitter {
        eu,
        next_register,
        width: plan.lanes,
        instructions: Vec::new(),
        zero: None,
        ones: None,
    };
    let (mask, predicate_instructions) = emitter.emit(&plan.predicate);
    let preheader = eu.blocks.get_mut(&plan.preheader).unwrap();
    preheader.instructions.extend(predicate_instructions);

    let count = fresh_register(eu, next_register, plan.count_type);
    let first_narrow = fresh_register(
        eu,
        next_register,
        unsigned_type(UnaryOp::CountTrailingZeros.result_width(plan.lanes)),
    );
    let nonempty = fresh_register(eu, next_register, unsigned_type(1));
    eu.blocks
        .get_mut(&plan.preheader)
        .unwrap()
        .instructions
        .extend([
            SIRInstruction::Unary(count, UnaryOp::PopCount, mask),
            SIRInstruction::Unary(first_narrow, UnaryOp::CountTrailingZeros, mask),
            SIRInstruction::Unary(nonempty, UnaryOp::Or, mask),
        ]);

    let first = if plan.best_type.width() == eu.register_map[&first_narrow].width() {
        first_narrow
    } else {
        let padding = fresh_register(
            eu,
            next_register,
            unsigned_type(plan.best_type.width() - eu.register_map[&first_narrow].width()),
        );
        let widened = fresh_register(eu, next_register, plan.best_type.clone());
        eu.blocks
            .get_mut(&plan.preheader)
            .unwrap()
            .instructions
            .extend([
                SIRInstruction::Imm(padding, SIRValue::new(0u8)),
                SIRInstruction::Concat(widened, vec![padding, first_narrow]),
            ]);
        widened
    };
    let one = fresh_register(eu, next_register, plan.found_type.clone());
    let best = fresh_register(eu, next_register, plan.best_type);
    let found = fresh_register(eu, next_register, plan.found_type);
    let preheader = eu.blocks.get_mut(&plan.preheader).unwrap();
    preheader.instructions.extend([
        SIRInstruction::Imm(one, SIRValue::new(1u8)),
        SIRInstruction::Mux(best, nonempty, first, plan.no_match_best),
        SIRInstruction::Mux(found, nonempty, one, plan.no_match_found),
    ]);
    let mut arguments = plan.exit_arguments;
    arguments[plan.exit_count_position] = count;
    arguments[plan.exit_best_position] = best;
    arguments[plan.exit_found_position] = found;
    preheader.terminator = SIRTerminator::Jump(plan.exit, arguments);

    for block in plan.loop_blocks {
        eu.blocks.remove(&block);
    }
}

pub(super) fn replace_sparse_loop_use(
    instruction: &mut SIRInstruction<RegionedAbsoluteAddr>,
    old: RegisterId,
    new: RegisterId,
) {
    instruction.for_each_use_mut(|register| {
        if *register == old {
            *register = new;
        }
    });
}

pub(super) fn replace_instruction_definition(
    instruction: &mut SIRInstruction<RegionedAbsoluteAddr>,
    definition: RegisterId,
) {
    *instruction
        .defined_register_mut()
        .expect("only an instruction that defines a register is renamed") = definition;
}

pub(super) fn is_lane_comparison(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::LtU
            | BinaryOp::LtS
            | BinaryOp::LeU
            | BinaryOp::LeS
            | BinaryOp::GtU
            | BinaryOp::GtS
            | BinaryOp::GeU
            | BinaryOp::GeS
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn emit_boolean_mux(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    next_register: &mut usize,
    emitted: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
    register_type: RegisterType,
    destination: RegisterId,
    condition: RegisterId,
    true_value: RegisterId,
    false_value: RegisterId,
) {
    let inverted = fresh_register(eu, next_register, register_type.clone());
    let on_true = fresh_register(eu, next_register, register_type.clone());
    let on_false = fresh_register(eu, next_register, register_type);
    emitted.extend([
        SIRInstruction::Unary(inverted, UnaryOp::BitNot, condition),
        SIRInstruction::Binary(on_true, condition, BinaryOp::And, true_value),
        SIRInstruction::Binary(on_false, inverted, BinaryOp::And, false_value),
        SIRInstruction::Binary(destination, on_true, BinaryOp::Or, on_false),
    ]);
}

pub(super) fn apply_bit_map_loop(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: BitMapLoopPlan,
    next_register: &mut usize,
) {
    let source_instructions = eu.blocks[&plan.header].instructions.clone();
    let source_definitions = source_instructions
        .iter()
        .enumerate()
        .filter_map(|(index, instruction)| def_reg(instruction).map(|register| (register, index)))
        .collect::<HashMap<_, _>>();
    let index_type = eu.register_map[&plan.index].clone();
    let mut emitted = Vec::new();
    let mut lane_bits = Vec::with_capacity(plan.lanes);
    for lane in 0..plan.lanes {
        let lane_index = fresh_register(eu, next_register, index_type.clone());
        emitted.push(SIRInstruction::Imm(lane_index, SIRValue::new(lane)));
        let mut replacements = HashMap::default();
        replacements.insert(plan.index, lane_index);
        for &instruction_index in &plan.dependency_indices {
            let source = &source_instructions[instruction_index];
            let old_definition = def_reg(source).expect("bit-map dependency defines a value");
            let definition_type = eu.register_map[&old_definition].clone();
            let new_definition = fresh_register(eu, next_register, definition_type.clone());
            let mapped =
                |register: RegisterId| replacements.get(&register).copied().unwrap_or(register);
            if let SIRInstruction::Binary(_, lhs, operation, rhs) = source
                && is_lane_comparison(*operation)
            {
                let mux_operand = [(*lhs, *rhs, true), (*rhs, *lhs, false)]
                    .into_iter()
                    .find_map(|(candidate, other, mux_is_lhs)| {
                        let index = *source_definitions.get(&candidate)?;
                        let SIRInstruction::Mux(_, condition, on_true, on_false) =
                            &source_instructions[index]
                        else {
                            return None;
                        };
                        Some((*condition, *on_true, *on_false, other, mux_is_lhs))
                    });
                if let Some((condition, on_true, on_false, other, mux_is_lhs)) = mux_operand {
                    let true_compare = fresh_register(eu, next_register, definition_type.clone());
                    let false_compare = fresh_register(eu, next_register, definition_type.clone());
                    let (true_lhs, true_rhs, false_lhs, false_rhs) = if mux_is_lhs {
                        (
                            mapped(on_true),
                            mapped(other),
                            mapped(on_false),
                            mapped(other),
                        )
                    } else {
                        (
                            mapped(other),
                            mapped(on_true),
                            mapped(other),
                            mapped(on_false),
                        )
                    };
                    emitted.extend([
                        SIRInstruction::Binary(true_compare, true_lhs, *operation, true_rhs),
                        SIRInstruction::Binary(false_compare, false_lhs, *operation, false_rhs),
                    ]);
                    emit_boolean_mux(
                        eu,
                        next_register,
                        &mut emitted,
                        definition_type,
                        new_definition,
                        mapped(condition),
                        true_compare,
                        false_compare,
                    );
                    replacements.insert(old_definition, new_definition);
                    continue;
                }
            }
            let mut cloned = source.clone();
            for (&old, &new) in &replacements {
                replace_sparse_loop_use(&mut cloned, old, new);
            }
            replace_instruction_definition(&mut cloned, new_definition);
            if let SIRInstruction::Mux(_, condition, true_value, false_value) = &cloned
                && definition_type.width() == 1
            {
                emit_boolean_mux(
                    eu,
                    next_register,
                    &mut emitted,
                    definition_type,
                    new_definition,
                    *condition,
                    *true_value,
                    *false_value,
                );
                replacements.insert(old_definition, new_definition);
                continue;
            }
            replacements.insert(old_definition, new_definition);
            emitted.push(cloned);
        }
        lane_bits.push(replacements[&plan.bit]);
    }
    lane_bits.reverse();
    let packed = fresh_register(eu, next_register, unsigned_type(plan.lanes));
    emitted.push(SIRInstruction::Concat(packed, lane_bits));
    let accumulator_width = plan.accumulator_type.width();
    let result = if accumulator_width == plan.lanes {
        packed
    } else {
        let padding = fresh_register(
            eu,
            next_register,
            unsigned_type(accumulator_width - plan.lanes),
        );
        emitted.push(SIRInstruction::Imm(padding, SIRValue::new(0u8)));
        let extended = fresh_register(eu, next_register, plan.accumulator_type.clone());
        emitted.push(SIRInstruction::Concat(extended, vec![padding, packed]));
        let low_mask = (BigUint::from(1u8) << plan.lanes) - BigUint::from(1u8);
        let mask = fresh_register(eu, next_register, plan.accumulator_type.clone());
        emitted.push(SIRInstruction::Imm(mask, SIRValue::new(low_mask)));
        let inverted_mask = fresh_register(eu, next_register, plan.accumulator_type.clone());
        emitted.push(SIRInstruction::Unary(inverted_mask, UnaryOp::BitNot, mask));
        let preserved = fresh_register(eu, next_register, plan.accumulator_type.clone());
        emitted.push(SIRInstruction::Binary(
            preserved,
            plan.initial_accumulator,
            BinaryOp::And,
            inverted_mask,
        ));
        let merged = fresh_register(eu, next_register, plan.accumulator_type);
        emitted.push(SIRInstruction::Binary(
            merged,
            preserved,
            BinaryOp::Or,
            extended,
        ));
        merged
    };
    let mut exit_arguments = plan.exit_arguments;
    exit_arguments[plan.exit_result_position] = result;
    let preheader = eu.blocks.get_mut(&plan.preheader).unwrap();
    preheader.instructions.extend(emitted);
    preheader.terminator = SIRTerminator::Jump(plan.exit, exit_arguments);
    eu.blocks.remove(&plan.header);
}

pub(super) fn apply_sparse_bitmap_loop(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: SparseBitmapLoopPlan,
    next_register: &mut usize,
) {
    let emitter = ExpressionEmitter {
        eu,
        next_register,
        width: plan.lanes,
        instructions: Vec::new(),
        zero: None,
        ones: None,
    };
    let (mask, predicate_instructions) = emitter.emit(&plan.predicate);
    let nonempty = fresh_register(eu, next_register, unsigned_type(1));
    let index_type = eu.register_map[&eu.blocks[&plan.header].params[plan.index_position]].clone();
    let index_width = index_type.width();
    let zero_index = fresh_register(eu, next_register, index_type.clone());
    let mut entry_arguments = plan.entry_arguments.clone();
    entry_arguments[plan.count_position] = mask;
    entry_arguments[plan.index_position] = zero_index;
    let preheader = eu.blocks.get_mut(&plan.preheader).unwrap();
    preheader.instructions.extend(predicate_instructions);
    preheader.instructions.extend([
        SIRInstruction::Unary(nonempty, UnaryOp::Or, mask),
        SIRInstruction::Imm(zero_index, SIRValue::new(0u8)),
    ]);
    preheader.terminator = SIRTerminator::Branch {
        cond: nonempty,
        true_block: (plan.header, entry_arguments),
        false_block: (plan.exit, plan.bypass_arguments.clone()),
    };

    let header_params = eu.blocks[&plan.header].params.clone();
    let remaining = header_params[plan.count_position];
    let old_index = header_params[plan.index_position];
    eu.register_map.insert(remaining, unsigned_type(plan.lanes));
    let trailing_width = UnaryOp::CountTrailingZeros.result_width(plan.lanes);
    let index_wide = fresh_register(eu, next_register, unsigned_type(trailing_width));
    let index = fresh_register(eu, next_register, index_type);
    let index_padding = (index_width > trailing_width).then(|| {
        fresh_register(
            eu,
            next_register,
            unsigned_type(index_width - trailing_width),
        )
    });
    let known_true = fresh_register(eu, next_register, unsigned_type(1));
    let one = fresh_register(eu, next_register, unsigned_type(plan.lanes));
    let remaining_minus_one = fresh_register(eu, next_register, unsigned_type(plan.lanes));
    let next_remaining = fresh_register(eu, next_register, unsigned_type(plan.lanes));
    let more = fresh_register(eu, next_register, unsigned_type(1));

    let mut old_instructions =
        std::mem::take(&mut eu.blocks.get_mut(&plan.header).unwrap().instructions);
    for instruction in &mut old_instructions {
        replace_sparse_loop_use(instruction, old_index, index);
        replace_sparse_loop_use(instruction, plan.common_predicate, known_true);
    }
    let header = eu.blocks.get_mut(&plan.header).unwrap();
    header.instructions.push(SIRInstruction::Unary(
        index_wide,
        UnaryOp::CountTrailingZeros,
        remaining,
    ));
    if let Some(padding) = index_padding {
        header
            .instructions
            .push(SIRInstruction::Imm(padding, SIRValue::new(0u8)));
        header
            .instructions
            .push(SIRInstruction::Concat(index, vec![padding, index_wide]));
    } else if index_width == trailing_width {
        header
            .instructions
            .push(SIRInstruction::Unary(index, UnaryOp::Ident, index_wide));
    } else {
        header
            .instructions
            .push(SIRInstruction::Slice(index, index_wide, 0, index_width));
    }
    header
        .instructions
        .push(SIRInstruction::Imm(known_true, SIRValue::new(1u8)));
    header.instructions.append(&mut old_instructions);
    header.instructions.extend([
        SIRInstruction::Imm(one, SIRValue::new(1u8)),
        SIRInstruction::Binary(remaining_minus_one, remaining, BinaryOp::Sub, one),
        SIRInstruction::Binary(
            next_remaining,
            remaining,
            BinaryOp::And,
            remaining_minus_one,
        ),
        SIRInstruction::Unary(more, UnaryOp::Or, next_remaining),
    ]);
    let mut backedge = plan.backedge_arguments;
    backedge[plan.count_position] = next_remaining;
    backedge[plan.index_position] = zero_index;
    header.terminator = SIRTerminator::Branch {
        cond: more,
        true_block: (plan.header, backedge),
        false_block: (plan.exit, plan.exit_arguments),
    };
}

pub(super) fn apply_last_payload_scan(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    plan: LastPayloadScanPlan,
    next_register: &mut usize,
    next_block: &mut usize,
) {
    let selected_block = fresh_block(next_block);
    let empty_block = fresh_block(next_block);
    let emitter = ExpressionEmitter {
        eu,
        next_register,
        width: plan.lanes,
        instructions: Vec::new(),
        zero: None,
        ones: None,
    };
    let (mask, predicate_instructions) = emitter.emit(&plan.predicate);
    let nonempty = fresh_register(eu, next_register, unsigned_type(1));
    let preheader = eu.blocks.get_mut(&plan.preheader).unwrap();
    preheader.instructions.extend(plan.hoisted_instructions);
    preheader.instructions.extend(predicate_instructions);
    preheader
        .instructions
        .push(SIRInstruction::Unary(nonempty, UnaryOp::Or, mask));
    preheader.terminator = SIRTerminator::Branch {
        cond: nonempty,
        true_block: (selected_block, Vec::new()),
        false_block: (empty_block, Vec::new()),
    };

    let count_width = UnaryOp::CountLeadingZeros.result_width(plan.lanes);
    let leading_zeros = fresh_register(eu, next_register, unsigned_type(count_width));
    let last_lane = fresh_register(eu, next_register, unsigned_type(count_width));
    let last_lane_number = fresh_register(eu, next_register, unsigned_type(count_width));
    let found = fresh_register(eu, next_register, plan.found_type.clone());
    let mut selected_instructions = vec![
        SIRInstruction::Unary(leading_zeros, UnaryOp::CountLeadingZeros, mask),
        SIRInstruction::Imm(last_lane, SIRValue::new(plan.lanes - 1)),
        SIRInstruction::Binary(last_lane_number, last_lane, BinaryOp::Sub, leading_zeros),
        SIRInstruction::Imm(found, SIRValue::new(1u8)),
    ];
    let mut selected_arguments = plan.exit_arguments.clone();
    selected_arguments[plan.exit_found_position] = found;
    for payload in &plan.payloads {
        let value = fresh_register(eu, next_register, payload.value_type.clone());
        selected_instructions.push(SIRInstruction::Load(
            value,
            payload.address,
            SIROffset::Element {
                index: last_lane_number,
                element_width: payload.element_width,
                bit_offset: payload.bit_offset,
                dynamic_bit_offset: None,
            },
            payload.width,
        ));
        selected_arguments[payload.exit_position] = value;
    }
    let selected = BasicBlock {
        id: selected_block,
        params: Vec::new(),
        instructions: selected_instructions,
        terminator: SIRTerminator::Jump(plan.exit, selected_arguments),
    };

    let mut empty_arguments = plan.exit_arguments;
    empty_arguments[plan.exit_found_position] = plan.no_match_found;
    for payload in &plan.payloads {
        empty_arguments[payload.exit_position] = payload.no_match;
    }
    let empty = BasicBlock {
        id: empty_block,
        params: Vec::new(),
        instructions: Vec::new(),
        terminator: SIRTerminator::Jump(plan.exit, empty_arguments),
    };

    for block in plan.loop_blocks {
        eu.blocks.remove(&block);
    }
    eu.blocks.insert(selected_block, selected);
    eu.blocks.insert(empty_block, empty);
}
