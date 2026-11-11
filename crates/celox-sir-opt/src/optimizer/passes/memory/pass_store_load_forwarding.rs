use super::pass_manager::ExecutionUnitPass;
use super::shared::{resolve_transitive_aliases, sir_value_to_u64};
use crate::HashMap;
use crate::PassOptions;
use crate::ir::*;
use std::collections::BTreeMap;

pub(in crate::optimizer) struct StoreLoadForwardingPass;

impl ExecutionUnitPass for StoreLoadForwardingPass {
    fn name(&self) -> &'static str {
        "store_load_forwarding"
    }

    fn handles_calls(&self) -> bool {
        true
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        let register_map = &eu.register_map;
        let mut aliases = HashMap::default();
        for block in eu.blocks.values_mut() {
            aliases.extend(forward_and_simplify(
                &mut block.instructions,
                register_map,
                options.four_state,
            ));
        }

        // A value simplified in its defining block can be consumed in any
        // dominated block. Apply the complete alias relation to the whole EU
        // before DCE; applying it only to the defining block leaves dangling
        // cross-block uses.
        let aliases = resolve_transitive_aliases(&aliases);
        for block in eu.blocks.values_mut() {
            for instruction in &mut block.instructions {
                apply_aliases_to_inst(instruction, &aliases);
            }
            block.terminator.for_each_use_mut(|register| {
                if let Some(&alias) = aliases.get(register) {
                    *register = alias;
                }
            });
        }
        super::super::dataflow::pass_vectorize_concat::remove_dead_definitions(eu);
    }
}

#[derive(Debug, Clone, Copy)]
enum BooleanExpression {
    Same(RegisterId),
    Not(RegisterId),
    And(RegisterId, RegisterId),
}

fn boolean_root(
    definitions: &HashMap<RegisterId, BooleanExpression>,
    mut register: RegisterId,
) -> RegisterId {
    let mut seen = crate::HashSet::default();
    while seen.insert(register) {
        match definitions.get(&register) {
            Some(BooleanExpression::Same(source)) => register = *source,
            _ => break,
        }
    }
    register
}

fn one_bit(register_map: &HashMap<RegisterId, RegisterType>, register: RegisterId) -> bool {
    register_map
        .get(&register)
        .is_some_and(|ty| ty.width() == 1)
}

fn boolean_complements(
    definitions: &HashMap<RegisterId, BooleanExpression>,
    left: RegisterId,
    right: RegisterId,
) -> bool {
    let left = boolean_root(definitions, left);
    let right = boolean_root(definitions, right);
    matches!(
        definitions.get(&left),
        Some(BooleanExpression::Not(source))
            if boolean_root(definitions, *source) == right
    ) || matches!(
        definitions.get(&right),
        Some(BooleanExpression::Not(source))
            if boolean_root(definitions, *source) == left
    )
}

fn complemented_and_common_factor(
    definitions: &HashMap<RegisterId, BooleanExpression>,
    left: RegisterId,
    right: RegisterId,
) -> Option<RegisterId> {
    let left = boolean_root(definitions, left);
    let right = boolean_root(definitions, right);
    let (
        Some(BooleanExpression::And(left_a, left_b)),
        Some(BooleanExpression::And(right_a, right_b)),
    ) = (definitions.get(&left), definitions.get(&right))
    else {
        return None;
    };
    [
        (*left_a, *left_b, *right_a, *right_b),
        (*left_a, *left_b, *right_b, *right_a),
        (*left_b, *left_a, *right_a, *right_b),
        (*left_b, *left_a, *right_b, *right_a),
    ]
    .into_iter()
    .find_map(|(common_left, other_left, common_right, other_right)| {
        (common_left == common_right && boolean_complements(definitions, other_left, other_right))
            .then_some(common_left)
    })
}

/// Per-block store-load forwarding + algebraic simplification.
/// Marks forwarded loads as dead by turning them into identity aliases.
fn forward_and_simplify(
    instructions: &mut [SIRInstruction<RegionedAbsoluteAddr>],
    register_map: &HashMap<RegisterId, RegisterType>,
    four_state: bool,
) -> HashMap<RegisterId, RegisterId> {
    struct StoreEntry {
        src: RegisterId,
        width: usize,
    }

    // Entries for one address are non-overlapping. Containing-definition
    // queries and invalidation visit only the predecessor and overlapping
    // ranges instead of rescanning every Store in the block.
    let mut known_stores: HashMap<RegionedAbsoluteAddr, BTreeMap<usize, StoreEntry>> =
        HashMap::default();
    let mut known_constants: HashMap<RegisterId, u64> = HashMap::default();
    let mut aliases: HashMap<RegisterId, RegisterId> = HashMap::default();
    let mut boolean_definitions = HashMap::<RegisterId, BooleanExpression>::default();

    for inst in instructions.iter_mut() {
        // Keep every newly recorded expression in canonical operands. Aliases
        // are always inserted after this rewrite, so one lookup is sufficient.
        apply_aliases_to_inst(inst, &aliases);
        match inst {
            // An outlined native kernel can change any state.
            _ if inst.accesses_unknown_state() => {
                known_stores.clear();
            }
            // A function call changes what its effects write.
            SIRInstruction::Call { effects, .. } => {
                for addr in &effects.writes {
                    known_stores.remove(addr);
                }
            }
            SIRInstruction::Store(addr, offset, width, src, _triggers, _)
                if offset.constant_bit_offset().is_some() =>
            {
                let off = offset.constant_bit_offset().unwrap();
                let store_end = off.saturating_add(*width);
                let ranges = known_stores.entry(*addr).or_default();
                let mut overlapping = Vec::new();
                if let Some((&start, entry)) = ranges.range(..=off).next_back()
                    && start.saturating_add(entry.width) > off
                {
                    overlapping.push(start);
                }
                overlapping.extend(ranges.range(off..store_end).map(|(&start, _)| start));
                overlapping.sort_unstable();
                overlapping.dedup();
                for start in overlapping {
                    ranges.remove(&start);
                }
                ranges.insert(
                    off,
                    StoreEntry {
                        src: *src,
                        width: *width,
                    },
                );
            }
            SIRInstruction::Store(
                addr,
                SIROffset::Dynamic(_) | SIROffset::Element { .. },
                _,
                _,
                _,
                _,
            ) => {
                // Conservatively invalidate all entries for this addr
                known_stores.remove(addr);
            }
            SIRInstruction::Load(dst, addr, offset, width)
                if offset.constant_bit_offset().is_some() =>
            {
                let off = offset.constant_bit_offset().unwrap();
                let load_end = off.saturating_add(*width);
                if let Some((&store_offset, entry)) = known_stores
                    .get(addr)
                    .and_then(|ranges| ranges.range(..=off).next_back())
                    && load_end <= store_offset.saturating_add(entry.width)
                {
                    if store_offset == off
                        && entry.width == *width
                        && forwarding_types_are_compatible(
                            register_map.get(dst),
                            register_map.get(&entry.src),
                            *width,
                            four_state,
                        )
                    {
                        // Forward: alias dst to the stored register
                        aliases.insert(*dst, entry.src);
                    } else if forwarding_slice_types_are_compatible(
                        register_map.get(dst),
                        register_map.get(&entry.src),
                        *width,
                        entry.width,
                        four_state,
                    ) {
                        *inst = SIRInstruction::Slice(*dst, entry.src, off - store_offset, *width);
                    }
                }
            }
            SIRInstruction::Imm(dst, val) => {
                if let Some(v) = sir_value_to_u64(val) {
                    known_constants.insert(*dst, v);
                }
            }
            SIRInstruction::Binary(dst, lhs, op, rhs) => {
                let lhs_const = known_constants.get(lhs).copied();
                let rhs_const = known_constants.get(rhs).copied();

                if !four_state
                    && one_bit(register_map, *dst)
                    && one_bit(register_map, *lhs)
                    && one_bit(register_map, *rhs)
                {
                    match op {
                        BinaryOp::And | BinaryOp::LogicAnd => {
                            let canonical_lhs = boolean_root(&boolean_definitions, *lhs);
                            let canonical_rhs = boolean_root(&boolean_definitions, *rhs);
                            if canonical_lhs == canonical_rhs
                                && register_map.get(dst) == register_map.get(lhs)
                            {
                                aliases.insert(*dst, *lhs);
                            } else {
                                boolean_definitions.insert(
                                    *dst,
                                    BooleanExpression::And(canonical_lhs, canonical_rhs),
                                );
                            }
                        }
                        BinaryOp::Or | BinaryOp::LogicOr => {
                            if lhs == rhs {
                                aliases.insert(*dst, *lhs);
                            } else if let Some(common) =
                                complemented_and_common_factor(&boolean_definitions, *lhs, *rhs)
                            {
                                // (x & y) | (x & !y) == x
                                if register_map.get(dst) == register_map.get(&common) {
                                    aliases.insert(*dst, common);
                                } else {
                                    boolean_definitions
                                        .insert(*dst, BooleanExpression::Same(common));
                                }
                            }
                        }
                        _ => {}
                    }
                }

                match (op, lhs_const, rhs_const) {
                    // shift by 0 → identity
                    (BinaryOp::Shr | BinaryOp::Shl | BinaryOp::Sar, _, Some(0)) => {
                        if register_map.get(dst) == register_map.get(lhs) {
                            aliases.insert(*dst, *lhs);
                        }
                    }
                    // These operations normalize Z to X (and arithmetic
                    // propagates unknowns), so only two-state inputs are identities.
                    (BinaryOp::Or | BinaryOp::Add | BinaryOp::Xor, _, Some(0))
                        if !four_state
                            || matches!(register_map.get(lhs), Some(RegisterType::Bit { .. })) =>
                    {
                        if register_map.get(dst) == register_map.get(lhs) {
                            aliases.insert(*dst, *lhs);
                        }
                    }
                    (BinaryOp::Or | BinaryOp::Add | BinaryOp::Xor, Some(0), _)
                        if !four_state
                            || matches!(register_map.get(rhs), Some(RegisterType::Bit { .. })) =>
                    {
                        if register_map.get(dst) == register_map.get(rhs) {
                            aliases.insert(*dst, *rhs);
                        }
                    }
                    // and with all-ones mask → identity (check if mask matches dst width)
                    (BinaryOp::And, _, Some(mask))
                        if mask == u64::MAX
                            || (mask > 0 && mask.count_ones() == mask.trailing_ones()) =>
                    {
                        // Only alias if mask covers all bits — we can't easily
                        // know the bit width of lhs here, so only handle the
                        // common case where the And itself produces a result
                        // that is exactly the masked width.
                        // This is conservative: we skip if unsure.
                    }
                    (BinaryOp::And, _, Some(_)) => {
                        // Actually, let's just check the specific pattern:
                        // If the And mask is all-ones for the width of the result,
                        // this is identity. But we don't have the result width
                        // readily available. Keep it simple: don't alias And here.
                        // The BitExtractPeepholePass handles the important cases.
                    }
                    _ => {}
                }
            }
            SIRInstruction::Unary(dst, UnaryOp::And | UnaryOp::Or | UnaryOp::Xor, src) => {
                // A reduction over one known bit is the bit itself. A Z bit
                // becomes X, so retain reductions on four-state inputs.
                // Require the exact register type so
                // replacing the unsigned reduction result cannot expose the
                // signedness of a one-bit source to later width extension.
                if register_map.get(src).is_some_and(|ty| ty.width() == 1)
                    && register_map.get(dst) == register_map.get(src)
                    && (!four_state
                        || matches!(register_map.get(src), Some(RegisterType::Bit { .. })))
                {
                    aliases.insert(*dst, *src);
                }
            }
            SIRInstruction::Unary(dst, UnaryOp::Ident, src)
                if register_map.get(dst) == register_map.get(src) =>
            {
                aliases.insert(*dst, *src);
            }
            SIRInstruction::Unary(dst, UnaryOp::ToTwoState, src)
                if !four_state && one_bit(register_map, *dst) && one_bit(register_map, *src) =>
            {
                if register_map.get(dst) == register_map.get(src) {
                    aliases.insert(*dst, *src);
                } else {
                    boolean_definitions.insert(*dst, BooleanExpression::Same(*src));
                }
            }
            SIRInstruction::Unary(dst, UnaryOp::LogicNot, src)
                if !four_state && one_bit(register_map, *dst) && one_bit(register_map, *src) =>
            {
                if let Some(BooleanExpression::Not(inner)) = boolean_definitions.get(src) {
                    let inner = boolean_root(&boolean_definitions, *inner);
                    if register_map.get(dst) == register_map.get(&inner) {
                        aliases.insert(*dst, inner);
                    } else {
                        boolean_definitions.insert(*dst, BooleanExpression::Same(inner));
                    }
                } else {
                    boolean_definitions.insert(
                        *dst,
                        BooleanExpression::Not(boolean_root(&boolean_definitions, *src)),
                    );
                }
            }
            SIRInstruction::Mux(dst, _, then_value, else_value)
                if then_value == else_value
                    && register_map.get(dst) == register_map.get(then_value) =>
            {
                aliases.insert(*dst, *then_value);
            }
            SIRInstruction::Commit(_, dst_addr, _, _, _) => {
                // A Commit is a write to its destination for every offset
                // representation, including PackedElements.
                known_stores.remove(dst_addr);
            }
            _ => {}
        }
    }

    if aliases.is_empty() {
        return aliases;
    }

    // Resolve transitive aliases
    let resolved = resolve_transitive_aliases(&aliases);

    // Apply aliases to all instruction operands
    for inst in instructions.iter_mut() {
        apply_aliases_to_inst(inst, &resolved);
    }
    resolved
}

/// Whether replacing a memory round trip with the stored SSA value preserves
/// the value interpretation expected by every user of the Load.
///
/// SLT lowering conservatively gives memory Loads a `Logic` result even in a
/// two-state simulation. A known two-state, unsigned `Bit` value stored at the
/// exact same width has the same payload as that Load, so retaining the memory
/// round trip solely for the register-kind difference is unnecessary. Signed
/// `Bit` values are excluded because replacing an unsigned `Logic` operand
/// with one could affect a later width extension. In four-state mode the
/// register kind also carries mask semantics and must match exactly.
fn forwarding_types_are_compatible(
    load: Option<&RegisterType>,
    stored: Option<&RegisterType>,
    width: usize,
    four_state: bool,
) -> bool {
    let (Some(load), Some(stored)) = (load, stored) else {
        return false;
    };
    if load.width() != width || stored.width() != width {
        return false;
    }
    if load == stored {
        return true;
    }
    !four_state
        && matches!(load, RegisterType::Logic { .. })
        && matches!(stored, RegisterType::Bit { signed: false, .. })
}

fn forwarding_slice_types_are_compatible(
    load: Option<&RegisterType>,
    stored: Option<&RegisterType>,
    load_width: usize,
    stored_width: usize,
    four_state: bool,
) -> bool {
    let (Some(load), Some(stored)) = (load, stored) else {
        return false;
    };
    if load.width() != load_width || stored.width() != stored_width {
        return false;
    }
    match (load, stored) {
        (RegisterType::Logic { .. }, RegisterType::Logic { .. }) => true,
        (
            RegisterType::Bit {
                signed: load_signed,
                ..
            },
            RegisterType::Bit {
                signed: stored_signed,
                ..
            },
        ) => load_signed == stored_signed,
        (RegisterType::Logic { .. }, RegisterType::Bit { signed: false, .. }) => !four_state,
        _ => false,
    }
}

fn apply_aliases_to_inst(
    inst: &mut SIRInstruction<RegionedAbsoluteAddr>,
    aliases: &HashMap<RegisterId, RegisterId>,
) {
    inst.for_each_use_mut(|register| {
        if let Some(&alias) = aliases.get(register) {
            *register = alias;
        }
    });
}

#[cfg(test)]
mod tests;
