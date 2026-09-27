//! Boolean aliases, edge facts, and predicate identity queries.

use super::*;

pub(super) fn resolve_boolean_alias(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mut register: RegisterId,
) -> (RegisterId, bool) {
    let mut inverted = false;
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let Some(&(block_id, idx)) = locations.get(&register) else {
            break;
        };
        match &eu.blocks[&block_id].instructions[idx] {
            SIRInstruction::Unary(
                _,
                crate::ir::UnaryOp::Ident | crate::ir::UnaryOp::ToTwoState,
                source,
            ) => {
                register = *source;
            }
            SIRInstruction::Unary(_, crate::ir::UnaryOp::LogicNot, source) => {
                register = *source;
                inverted = !inverted;
            }
            _ => break,
        }
    }
    (register, inverted)
}

pub(super) fn indexed_incoming_edges(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    graph: &SirCfg,
) -> Vec<Vec<(BlockId, Option<bool>)>> {
    let mut incoming = vec![Vec::new(); graph.block_ids.len()];
    for (target, predecessors) in graph.predecessors.iter().enumerate() {
        let target_id = graph.block_ids[target];
        for &predecessor in predecessors {
            let predecessor_id = graph.block_ids[predecessor];
            match &eu.blocks[&predecessor_id].terminator {
                SIRTerminator::Jump(destination, _) if *destination == target_id => {
                    incoming[target].push((predecessor_id, None));
                }
                SIRTerminator::Branch {
                    true_block,
                    false_block,
                    ..
                } => {
                    if true_block.0 == target_id {
                        incoming[target].push((predecessor_id, Some(true)));
                    }
                    if false_block.0 == target_id {
                        incoming[target].push((predecessor_id, Some(false)));
                    }
                }
                _ => {}
            }
        }
    }
    incoming
}

pub(super) fn facts_on_edge(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    predecessor: BlockId,
    target: BlockId,
    edge_truth: Option<bool>,
    facts: &HashMap<PathFactKey, bool>,
) -> Option<HashMap<PathFactKey, bool>> {
    let mut result = facts.clone();
    let block = eu.blocks.get(&predecessor)?;
    match (&block.terminator, edge_truth) {
        (SIRTerminator::Jump(destination, _), None) if *destination == target => {}
        (
            SIRTerminator::Branch {
                cond,
                true_block,
                false_block,
            },
            Some(truth),
        ) if (truth && true_block.0 == target) || (!truth && false_block.0 == target) => {
            let (root, inverted) = resolve_boolean_alias(eu, def_locations, *cond);
            let root_truth = truth ^ inverted;
            let register_key = PathFactKey::Register(root);
            if result
                .get(&register_key)
                .is_some_and(|known| *known != root_truth)
            {
                return None;
            }
            result.insert(register_key, root_truth);
            if let Some((predicate, predicate_inverted)) = predicate_key(eu, def_locations, *cond) {
                let predicate_truth = truth ^ predicate_inverted;
                let key = PathFactKey::Predicate(predicate);
                if result
                    .get(&key)
                    .is_some_and(|known| *known != predicate_truth)
                {
                    return None;
                }
                result.insert(key, predicate_truth);
            }
        }
        _ => return None,
    }
    Some(result)
}

pub(super) fn known_condition_truth(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    def_locations: &HashMap<RegisterId, (BlockId, usize)>,
    facts: &HashMap<PathFactKey, bool>,
    condition: RegisterId,
) -> Option<bool> {
    let (root, inverted) = resolve_boolean_alias(eu, def_locations, condition);
    if let Some(value) = facts.get(&PathFactKey::Register(root)) {
        return Some(*value ^ inverted);
    }
    let (predicate, predicate_inverted) = predicate_key(eu, def_locations, condition)?;
    let value = known_predicate_truth(facts, &predicate)?;
    Some(value ^ predicate_inverted)
}

fn known_predicate_truth(facts: &HashMap<PathFactKey, bool>, query: &PredicateKey) -> Option<bool> {
    if let Some(value) = facts.get(&PathFactKey::Predicate(query.clone())) {
        return Some(*value);
    }
    let same_lhs = |key: &PredicateKey| key.lhs == query.lhs;
    for (fact, &value) in facts {
        let PathFactKey::Predicate(fact) = fact else {
            continue;
        };
        if !same_lhs(fact) {
            continue;
        }
        if fact.kind == query.kind
            && fact.kind == PredicateKind::Equal
            && different_constants(&fact.rhs, &query.rhs)
            && value
        {
            return Some(false);
        }
        if fact.rhs == query.rhs && fact.kind != query.kind && value {
            return Some(false);
        }
        if fact.rhs == query.rhs && fact.kind != query.kind && !value {
            return Some(true);
        }
    }
    None
}

fn different_constants(left: &PredicateRhs, right: &PredicateRhs) -> bool {
    match (left, right) {
        (
            PredicateRhs::Constant(left_payload, left_mask),
            PredicateRhs::Constant(right_payload, right_mask),
        ) => {
            left_mask.iter().all(|word| *word == 0)
                && right_mask.iter().all(|word| *word == 0)
                && left_payload != right_payload
        }
        _ => false,
    }
}

pub(super) fn predicate_key(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mut register: RegisterId,
) -> Option<(PredicateKey, bool)> {
    let mut inverted = false;
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let &(block, index) = locations.get(&register)?;
        match &eu.blocks[&block].instructions[index] {
            SIRInstruction::Unary(_, crate::ir::UnaryOp::LogicNot, source) => {
                register = *source;
                inverted = !inverted;
            }
            SIRInstruction::Unary(
                _,
                crate::ir::UnaryOp::Ident | crate::ir::UnaryOp::ToTwoState,
                source,
            ) => register = *source,
            SIRInstruction::Binary(_, lhs, op, rhs)
                if matches!(
                    op,
                    crate::ir::BinaryOp::Eq
                        | crate::ir::BinaryOp::EqWildcard
                        | crate::ir::BinaryOp::Ne
                        | crate::ir::BinaryOp::NeWildcard
                ) =>
            {
                let kind = match op {
                    crate::ir::BinaryOp::Eq | crate::ir::BinaryOp::EqWildcard => {
                        PredicateKind::Equal
                    }
                    crate::ir::BinaryOp::Ne | crate::ir::BinaryOp::NeWildcard => {
                        PredicateKind::NotEqual
                    }
                    _ => unreachable!(),
                };
                let lhs = canonical_identity_register(eu, locations, *lhs);
                let rhs = if let Some(value) = immediate_value(eu, locations, *rhs) {
                    PredicateRhs::Constant(value.0, value.1)
                } else {
                    PredicateRhs::Register(canonical_identity_register(eu, locations, *rhs))
                };
                return Some((PredicateKey { lhs, kind, rhs }, inverted));
            }
            _ => return None,
        }
    }
    None
}

fn canonical_identity_register(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mut register: RegisterId,
) -> RegisterId {
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let Some(&(block, index)) = locations.get(&register) else {
            break;
        };
        match &eu.blocks[&block].instructions[index] {
            SIRInstruction::Unary(
                _,
                crate::ir::UnaryOp::Ident | crate::ir::UnaryOp::ToTwoState,
                source,
            ) => register = *source,
            _ => break,
        }
    }
    register
}

fn immediate_value(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
    locations: &HashMap<RegisterId, (BlockId, usize)>,
    mut register: RegisterId,
) -> Option<(Vec<u64>, Vec<u64>)> {
    let mut seen = HashSet::default();
    while seen.insert(register) {
        let &(block, index) = locations.get(&register)?;
        match &eu.blocks[&block].instructions[index] {
            SIRInstruction::Imm(_, value) => {
                return Some((value.payload.to_u64_digits(), value.mask.to_u64_digits()));
            }
            SIRInstruction::Unary(
                _,
                crate::ir::UnaryOp::Ident | crate::ir::UnaryOp::ToTwoState,
                source,
            ) => register = *source,
            _ => return None,
        }
    }
    None
}
