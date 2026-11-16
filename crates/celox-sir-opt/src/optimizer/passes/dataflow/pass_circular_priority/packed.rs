//! Packed expressions: building, interning and simplifying their nodes.

use super::*;

#[derive(Clone, Copy)]
pub(super) enum PackedOperation {
    Identity(RegisterId),
    Not(RegisterId),
    And(RegisterId, RegisterId),
    Or(RegisterId, RegisterId),
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_packed_expression(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    definitions: &HashMap<RegisterId, Definition>,
    constant_cache: &mut HashMap<RegisterId, Option<bool>>,
    loop_blocks: &HashSet<BlockId>,
    preheader: BlockId,
    index: RegisterId,
    root: RegisterId,
    invert: bool,
    lanes: usize,
    bit_array_elements: &HashMap<AbsoluteAddr, usize>,
    array_shapes: &HashMap<AbsoluteAddr, ArrayShape>,
    hoisted_values: &HashSet<RegisterId>,
) -> Option<PackedExpression> {
    let root = strip_boolean_identity(eu, definitions, root);
    let mut nodes = Vec::new();
    let mut interned = HashMap::<PackedNode, usize>::default();
    let mut values = HashMap::<RegisterId, usize>::default();
    let mut operations = HashMap::<RegisterId, PackedOperation>::default();
    let mut stack = vec![(root, false)];

    while let Some((register, expanded)) = stack.pop() {
        if values.contains_key(&register) {
            continue;
        }
        if !expanded {
            if let Some(value) = boolean_constant(eu, definitions, constant_cache, register) {
                let node = if value {
                    PackedNode::Ones
                } else {
                    PackedNode::Zero
                };
                values.insert(register, intern_node(&mut nodes, &mut interned, node));
                continue;
            }
            if eu.register_map.get(&register).map(RegisterType::width) != Some(1) {
                return None;
            }
            let definition = definitions.get(&register).copied()?;
            if !loop_blocks.contains(&definition.block()) {
                if !cfg.dominates(definition.block(), preheader) {
                    return None;
                }
                values.insert(
                    register,
                    intern_node(&mut nodes, &mut interned, PackedNode::Broadcast(register)),
                );
                continue;
            }
            let definition_instruction = instruction(eu, definitions, register)?;
            match definition_instruction {
                SIRInstruction::Load(_, address, offset, 1)
                    if is_lane_bit_offset(offset, index)
                        && bit_array_elements
                            .get(&address.absolute_addr())
                            .is_some_and(|&element_count| element_count >= lanes) =>
                {
                    values.insert(
                        register,
                        intern_node(
                            &mut nodes,
                            &mut interned,
                            PackedNode::Load {
                                address: *address,
                                unpacked_element_width: array_shapes
                                    .get(&address.absolute_addr())
                                    .map(|shape| shape.element_width),
                            },
                        ),
                    );
                }
                SIRInstruction::Unary(
                    _,
                    UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Or,
                    source,
                ) if eu.register_map.get(source).map(RegisterType::width) == Some(1) => {
                    operations.insert(register, PackedOperation::Identity(*source));
                    stack.push((register, true));
                    stack.push((*source, false));
                }
                SIRInstruction::Unary(_, UnaryOp::LogicNot | UnaryOp::BitNot, source) => {
                    operations.insert(register, PackedOperation::Not(*source));
                    stack.push((register, true));
                    stack.push((*source, false));
                }
                SIRInstruction::Binary(_, lhs, BinaryOp::LogicAnd | BinaryOp::And, rhs) => {
                    operations.insert(register, PackedOperation::And(*lhs, *rhs));
                    stack.push((register, true));
                    stack.push((*rhs, false));
                    stack.push((*lhs, false));
                }
                SIRInstruction::Binary(_, lhs, BinaryOp::LogicOr | BinaryOp::Or, rhs) => {
                    operations.insert(register, PackedOperation::Or(*lhs, *rhs));
                    stack.push((register, true));
                    stack.push((*rhs, false));
                    stack.push((*lhs, false));
                }
                SIRInstruction::Binary(_, lhs, BinaryOp::Eq | BinaryOp::EqWildcard, rhs)
                    if match_lane_eq(
                        eu,
                        cfg,
                        definitions,
                        loop_blocks,
                        preheader,
                        index,
                        lanes,
                        array_shapes,
                        hoisted_values,
                        *lhs,
                        *rhs,
                    )
                    .is_some() =>
                {
                    let node = match_lane_eq(
                        eu,
                        cfg,
                        definitions,
                        loop_blocks,
                        preheader,
                        index,
                        lanes,
                        array_shapes,
                        hoisted_values,
                        *lhs,
                        *rhs,
                    )?;
                    values.insert(register, intern_node(&mut nodes, &mut interned, node));
                }
                SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs)
                    if immediate(eu, definitions, *rhs) == Some(1) =>
                {
                    operations.insert(register, PackedOperation::Identity(*lhs));
                    stack.push((register, true));
                    stack.push((*lhs, false));
                }
                SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs)
                    if immediate(eu, definitions, *lhs) == Some(1) =>
                {
                    operations.insert(register, PackedOperation::Identity(*rhs));
                    stack.push((register, true));
                    stack.push((*rhs, false));
                }
                SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs)
                    if immediate(eu, definitions, *rhs) == Some(0) =>
                {
                    operations.insert(register, PackedOperation::Not(*lhs));
                    stack.push((register, true));
                    stack.push((*lhs, false));
                }
                SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs)
                    if immediate(eu, definitions, *lhs) == Some(0) =>
                {
                    operations.insert(register, PackedOperation::Not(*rhs));
                    stack.push((register, true));
                    stack.push((*rhs, false));
                }
                SIRInstruction::Binary(_, lhs, BinaryOp::LtU, rhs)
                    if index_prefix_lhs(eu, definitions, *lhs, index, lanes)
                        && definitions.get(rhs).is_some_and(|definition| {
                            cfg.dominates(definition.block(), preheader)
                        }) =>
                {
                    values.insert(
                        register,
                        intern_node(&mut nodes, &mut interned, PackedNode::Prefix(*rhs)),
                    );
                }
                _ => return None,
            }
            continue;
        }

        let operation = operations.get(&register).copied()?;
        let node = match operation {
            PackedOperation::Identity(source) => {
                values.insert(register, *values.get(&source)?);
                continue;
            }
            PackedOperation::Not(source) => simplify_not(&nodes, *values.get(&source)?),
            PackedOperation::And(lhs, rhs) => {
                simplify_and(&nodes, *values.get(&lhs)?, *values.get(&rhs)?)
            }
            PackedOperation::Or(lhs, rhs) => {
                simplify_or(&nodes, *values.get(&lhs)?, *values.get(&rhs)?)
            }
        };
        values.insert(register, intern_node(&mut nodes, &mut interned, node));
    }

    let root = *values.get(&root)?;
    let dynamic_loads = nodes
        .iter()
        .filter(|node| matches!(node, PackedNode::Load { .. } | PackedNode::LaneEq { .. }))
        .count();
    let value_ops = nodes
        .iter()
        .filter(|node| {
            matches!(
                node,
                PackedNode::Broadcast(_)
                    | PackedNode::Prefix(_)
                    | PackedNode::LaneEq { .. }
                    | PackedNode::Not(_)
                    | PackedNode::And(..)
                    | PackedNode::Or(..)
            )
        })
        .count();
    Some(PackedExpression {
        nodes,
        root,
        invert,
        dynamic_loads,
        value_ops,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn match_lane_eq(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    cfg: &SirCfg,
    definitions: &HashMap<RegisterId, Definition>,
    loop_blocks: &HashSet<BlockId>,
    preheader: BlockId,
    index: RegisterId,
    lanes: usize,
    array_shapes: &HashMap<AbsoluteAddr, ArrayShape>,
    hoisted_values: &HashSet<RegisterId>,
    lhs: RegisterId,
    rhs: RegisterId,
) -> Option<PackedNode> {
    for (loaded, value) in [(lhs, rhs), (rhs, lhs)] {
        let SIRInstruction::Load(
            _,
            address,
            SIROffset::Element {
                index: load_index,
                element_width,
                bit_offset,
                dynamic_bit_offset,
            },
            field_width,
        ) = instruction(eu, definitions, loaded)?
        else {
            continue;
        };
        let dynamic_bit_offset = if let Some(offset) = dynamic_bit_offset {
            immediate(eu, definitions, *offset)?
        } else {
            0
        };
        let dynamic_bit_offset = usize::try_from(dynamic_bit_offset).ok()?;
        let bit_offset = bit_offset.checked_add(dynamic_bit_offset)?;
        if *load_index != index
            || *field_width == 0
            || bit_offset.checked_add(*field_width)? > *element_width
            || eu.register_map.get(&value).map(RegisterType::width) != Some(*field_width)
        {
            continue;
        }
        let shape = array_shapes.get(&address.absolute_addr())?;
        if shape.element_width != *element_width || shape.element_count < lanes {
            continue;
        }
        let value_definition = definitions.get(&value).copied()?;
        let defined_in_loop = loop_blocks.contains(&value_definition.block());
        if defined_in_loop && !hoisted_values.contains(&value)
            || !defined_in_loop && !cfg.dominates(value_definition.block(), preheader)
        {
            continue;
        }
        element_width.checked_mul(lanes)?;
        field_width.checked_mul(lanes)?;
        return Some(PackedNode::LaneEq {
            address: *address,
            element_width: *element_width,
            bit_offset,
            field_width: *field_width,
            value,
        });
    }
    None
}

pub(super) fn is_lane_bit_offset(offset: &SIROffset, index: RegisterId) -> bool {
    matches!(offset, SIROffset::Dynamic(load_index) if *load_index == index)
        || matches!(
            offset,
            SIROffset::Element {
                index: load_index,
                element_width: 1,
                bit_offset: 0,
                dynamic_bit_offset: None,
            } if *load_index == index
        )
}

pub(super) fn index_prefix_lhs(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    mut value: RegisterId,
    index: RegisterId,
    lanes: usize,
) -> bool {
    let required_mask = lanes
        .checked_next_power_of_two()
        .and_then(|width| width.checked_sub(1))
        .and_then(|mask| u64::try_from(mask).ok());
    loop {
        if value == index {
            return true;
        }
        match instruction(eu, definitions, value) {
            Some(SIRInstruction::Unary(_, UnaryOp::Ident | UnaryOp::ToTwoState, source)) => {
                value = *source
            }
            Some(SIRInstruction::Binary(_, lhs, BinaryOp::Shr, rhs))
                if immediate(eu, definitions, *rhs) == Some(0) =>
            {
                value = *lhs;
            }
            Some(SIRInstruction::Binary(_, lhs, BinaryOp::And, rhs))
                if immediate(eu, definitions, *rhs)
                    .zip(required_mask)
                    .is_some_and(|(mask, required)| mask & required == required) =>
            {
                value = *lhs;
            }
            Some(SIRInstruction::Binary(_, lhs, BinaryOp::And, rhs))
                if immediate(eu, definitions, *lhs)
                    .zip(required_mask)
                    .is_some_and(|(mask, required)| mask & required == required) =>
            {
                value = *rhs;
            }
            Some(SIRInstruction::Concat(_, parts))
                if parts.last() == Some(&index)
                    && parts[..parts.len().saturating_sub(1)]
                        .iter()
                        .all(|&part| immediate(eu, definitions, part) == Some(0)) =>
            {
                value = index;
            }
            _ => return false,
        }
    }
}

pub(super) fn intern_node(
    nodes: &mut Vec<PackedNode>,
    interned: &mut HashMap<PackedNode, usize>,
    node: PackedNode,
) -> usize {
    if let Some(&index) = interned.get(&node) {
        return index;
    }
    let index = nodes.len();
    nodes.push(node.clone());
    interned.insert(node, index);
    index
}

pub(super) fn simplify_not(nodes: &[PackedNode], source: usize) -> PackedNode {
    match nodes[source] {
        PackedNode::Zero => PackedNode::Ones,
        PackedNode::Ones => PackedNode::Zero,
        PackedNode::Not(inner) => nodes[inner].clone(),
        _ => PackedNode::Not(source),
    }
}

pub(super) fn simplify_and(nodes: &[PackedNode], lhs: usize, rhs: usize) -> PackedNode {
    if lhs == rhs {
        return nodes[lhs].clone();
    }
    match (&nodes[lhs], &nodes[rhs]) {
        (PackedNode::Zero, _) | (_, PackedNode::Zero) => PackedNode::Zero,
        (PackedNode::Ones, other) | (other, PackedNode::Ones) => other.clone(),
        _ => PackedNode::And(lhs.min(rhs), lhs.max(rhs)),
    }
}

pub(super) fn simplify_or(nodes: &[PackedNode], lhs: usize, rhs: usize) -> PackedNode {
    if lhs == rhs {
        return nodes[lhs].clone();
    }
    match (&nodes[lhs], &nodes[rhs]) {
        (PackedNode::Ones, _) | (_, PackedNode::Ones) => PackedNode::Ones,
        (PackedNode::Zero, other) | (other, PackedNode::Zero) => other.clone(),
        _ => PackedNode::Or(lhs.min(rhs), lhs.max(rhs)),
    }
}

pub(super) fn boolean_constant(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    definitions: &HashMap<RegisterId, Definition>,
    cache: &mut HashMap<RegisterId, Option<bool>>,
    root: RegisterId,
) -> Option<bool> {
    if let Some(value) = cache.get(&root) {
        return *value;
    }
    let mut stack = vec![(root, false)];
    while let Some((register, expanded)) = stack.pop() {
        if cache.contains_key(&register) {
            continue;
        }
        if eu.register_map.get(&register).map(RegisterType::width) != Some(1) {
            cache.insert(register, None);
            continue;
        }
        let Some(definition) = instruction(eu, definitions, register) else {
            cache.insert(register, None);
            continue;
        };
        if !expanded {
            match definition {
                SIRInstruction::Imm(_, value) => {
                    cache.insert(
                        register,
                        sir_value_to_u64(value).map(|value| value & 1 != 0),
                    );
                }
                SIRInstruction::Unary(
                    _,
                    UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Or,
                    source,
                )
                | SIRInstruction::Unary(_, UnaryOp::LogicNot | UnaryOp::BitNot, source) => {
                    stack.push((register, true));
                    stack.push((*source, false));
                }
                SIRInstruction::Binary(
                    _,
                    lhs,
                    BinaryOp::LogicAnd
                    | BinaryOp::And
                    | BinaryOp::LogicOr
                    | BinaryOp::Or
                    | BinaryOp::Eq
                    | BinaryOp::Ne,
                    rhs,
                ) => {
                    stack.push((register, true));
                    stack.push((*rhs, false));
                    stack.push((*lhs, false));
                }
                _ => {
                    cache.insert(register, None);
                }
            }
            continue;
        }

        let value = match definition {
            SIRInstruction::Unary(
                _,
                UnaryOp::Ident | UnaryOp::ToTwoState | UnaryOp::Or,
                source,
            ) => cache.get(source).copied().flatten(),
            SIRInstruction::Unary(_, UnaryOp::LogicNot | UnaryOp::BitNot, source) => {
                cache.get(source).copied().flatten().map(|value| !value)
            }
            SIRInstruction::Binary(_, lhs, BinaryOp::LogicAnd | BinaryOp::And, rhs) => match (
                cache.get(lhs).copied().flatten(),
                cache.get(rhs).copied().flatten(),
            ) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            },
            SIRInstruction::Binary(_, lhs, BinaryOp::LogicOr | BinaryOp::Or, rhs) => match (
                cache.get(lhs).copied().flatten(),
                cache.get(rhs).copied().flatten(),
            ) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            },
            SIRInstruction::Binary(_, lhs, BinaryOp::Eq, rhs) => cache
                .get(lhs)
                .copied()
                .flatten()
                .zip(cache.get(rhs).copied().flatten())
                .map(|(lhs, rhs)| lhs == rhs),
            SIRInstruction::Binary(_, lhs, BinaryOp::Ne, rhs) => cache
                .get(lhs)
                .copied()
                .flatten()
                .zip(cache.get(rhs).copied().flatten())
                .map(|(lhs, rhs)| lhs != rhs),
            _ => None,
        };
        cache.insert(register, value);
    }
    cache.get(&root).copied().flatten()
}

pub(super) fn ids_available(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    plans: &[CircularPriorityPlan],
    linear_plans: &[LinearBitmapScanPlan],
    payload_plans: &[LastPayloadScanPlan],
    sparse_plans: &[SparseBitmapLoopPlan],
) -> bool {
    // A packed node normally emits one register. Prefix and Broadcast may emit
    // three, and materializing a constant root/inversion needs at most two
    // more. Keep this bound independent of the expression shape so ID-space
    // exhaustion is rejected before mutating the EU.
    let expression_registers = |expression: &PackedExpression, lanes: usize| {
        expression.nodes.iter().try_fold(2usize, |total, node| {
            let registers = if matches!(node, PackedNode::LaneEq { .. }) {
                lanes.checked_mul(2)?.checked_add(1)?
            } else {
                3
            };
            total.checked_add(registers)
        })
    };
    let required_registers = plans
        .iter()
        .try_fold(0usize, |total, plan| {
            total
                .checked_add(expression_registers(&plan.predicate, plan.lanes)?)?
                .checked_add(9)
        })
        .and_then(|total| {
            linear_plans.iter().try_fold(total, |total, plan| {
                total
                    .checked_add(expression_registers(&plan.predicate, plan.lanes)?)?
                    .checked_add(8)
            })
        })
        .and_then(|total| {
            payload_plans.iter().try_fold(total, |total, plan| {
                total
                    .checked_add(expression_registers(&plan.predicate, plan.lanes)?)?
                    .checked_add(plan.payloads.len())?
                    .checked_add(5)
            })
        })
        .and_then(|total| {
            sparse_plans.iter().try_fold(total, |total, plan| {
                total
                    .checked_add(expression_registers(&plan.predicate, plan.lanes)?)?
                    // CTZ may need one extra zero-extension register when the
                    // original loop index is wider than its result.
                    .checked_add(10)
            })
        });
    let required_blocks = plans
        .len()
        .checked_add(payload_plans.len())
        .and_then(|count| count.checked_mul(2));
    let (Some(required_registers), Some(required_blocks)) = (required_registers, required_blocks)
    else {
        return false;
    };
    let max_register = eu
        .register_map
        .keys()
        .map(|register| register.0)
        .max()
        .unwrap_or(0);
    let max_block = eu.blocks.keys().map(|block| block.0).max().unwrap_or(0);
    max_register.checked_add(required_registers).is_some()
        && max_block.checked_add(required_blocks).is_some()
}

pub(super) fn fresh_register(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    next_register: &mut usize,
    register_type: RegisterType,
) -> RegisterId {
    *next_register += 1;
    let register = RegisterId(*next_register);
    assert!(eu.register_map.insert(register, register_type).is_none());
    register
}

pub(super) fn fresh_block(next_block: &mut usize) -> BlockId {
    *next_block += 1;
    BlockId(*next_block)
}

pub(super) fn unsigned_type(width: usize) -> RegisterType {
    RegisterType::Bit {
        width,
        signed: false,
    }
}
