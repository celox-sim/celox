use super::pass_manager::ExecutionUnitPass;
use super::shared::{
    collect_all_used_registers, def_reg, resolve_transitive_aliases, sir_value_to_u64,
};
use crate::HashMap;
use crate::ir::*;
use crate::optimizer::PassOptions;
use std::collections::BTreeMap;

pub(super) struct StoreLoadForwardingPass;

impl ExecutionUnitPass for StoreLoadForwardingPass {
    fn name(&self) -> &'static str {
        "store_load_forwarding"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, options: &PassOptions) {
        let register_map = &eu.register_map;
        for block in eu.blocks.values_mut() {
            forward_and_simplify(&mut block.instructions, register_map, options.four_state);
        }

        // Apply aliases across the whole EU
        // (block params, terminators, all instructions)
        // then DCE
        dead_code_eliminate(eu);
    }
}

/// Per-block store-load forwarding + algebraic simplification.
/// Marks forwarded loads as dead by turning them into identity aliases.
fn forward_and_simplify(
    instructions: &mut [SIRInstruction<RegionedAbsoluteAddr>],
    register_map: &HashMap<RegisterId, RegisterType>,
    four_state: bool,
) {
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

    for inst in instructions.iter_mut() {
        match inst {
            SIRInstruction::Store(addr, SIROffset::Static(off), width, src, _triggers, _) => {
                let store_end = off.saturating_add(*width);
                let ranges = known_stores.entry(*addr).or_default();
                let mut overlapping = Vec::new();
                if let Some((&start, entry)) = ranges.range(..=*off).next_back()
                    && start.saturating_add(entry.width) > *off
                {
                    overlapping.push(start);
                }
                overlapping.extend(ranges.range(*off..store_end).map(|(&start, _)| start));
                overlapping.sort_unstable();
                overlapping.dedup();
                for start in overlapping {
                    ranges.remove(&start);
                }
                ranges.insert(
                    *off,
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
            SIRInstruction::Load(dst, addr, SIROffset::Static(off), width) => {
                let load_end = off.saturating_add(*width);
                if let Some((&store_offset, entry)) = known_stores
                    .get(addr)
                    .and_then(|ranges| ranges.range(..=*off).next_back())
                    && load_end <= store_offset.saturating_add(entry.width)
                {
                    if store_offset == *off
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
                        *inst = SIRInstruction::Slice(*dst, entry.src, *off - store_offset, *width);
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

                match (op, lhs_const, rhs_const) {
                    // shift by 0 → identity
                    (BinaryOp::Shr | BinaryOp::Shl | BinaryOp::Sar, _, Some(0)) => {
                        if register_map.get(dst) == register_map.get(lhs) {
                            aliases.insert(*dst, *lhs);
                        }
                    }
                    // or/add with 0 → identity
                    (BinaryOp::Or | BinaryOp::Add | BinaryOp::Xor, _, Some(0)) => {
                        if register_map.get(dst) == register_map.get(lhs) {
                            aliases.insert(*dst, *lhs);
                        }
                    }
                    (BinaryOp::Or | BinaryOp::Add | BinaryOp::Xor, Some(0), _) => {
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
                // A reduction over one bit is the bit itself, including an
                // unknown four-state bit. Require the exact register type so
                // replacing the unsigned reduction result cannot expose the
                // signedness of a one-bit source to later width extension.
                if register_map.get(src).is_some_and(|ty| ty.width() == 1)
                    && register_map.get(dst) == register_map.get(src)
                {
                    aliases.insert(*dst, *src);
                }
            }
            SIRInstruction::Commit(_, dst_addr, SIROffset::Static(_), _, _) => {
                // Invalidate known stores for the destination address
                known_stores.remove(dst_addr);
            }
            SIRInstruction::Commit(
                _,
                dst_addr,
                SIROffset::Dynamic(_) | SIROffset::Element { .. },
                _,
                _,
            ) => {
                known_stores.remove(dst_addr);
            }
            _ => {}
        }
    }

    if aliases.is_empty() {
        return;
    }

    // Resolve transitive aliases
    let resolved = resolve_transitive_aliases(&aliases);

    // Apply aliases to all instruction operands
    for inst in instructions.iter_mut() {
        apply_aliases_to_inst(inst, &resolved);
    }
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
    match inst {
        SIRInstruction::Imm(_, _) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            if let Some(&to) = aliases.get(lhs) {
                *lhs = to;
            }
            if let Some(&to) = aliases.get(rhs) {
                *rhs = to;
            }
        }
        SIRInstruction::Unary(_, _, src) => {
            if let Some(&to) = aliases.get(src) {
                *src = to;
            }
        }
        SIRInstruction::Load(_, _, offset, _) => {
            super::shared::replace_offset_registers(offset, aliases);
        }
        SIRInstruction::Store(_, offset, _, src, _, _) => {
            super::shared::replace_offset_registers(offset, aliases);
            if let Some(&to) = aliases.get(src) {
                *src = to;
            }
        }
        SIRInstruction::Commit(_, _, offset, _, _) => {
            super::shared::replace_offset_registers(offset, aliases);
        }
        SIRInstruction::Concat(_, args) => {
            for arg in args {
                if let Some(&to) = aliases.get(arg) {
                    *arg = to;
                }
            }
        }
        SIRInstruction::Mux(_, cond, then_val, else_val) => {
            if let Some(&to) = aliases.get(cond) {
                *cond = to;
            }
            if let Some(&to) = aliases.get(then_val) {
                *then_val = to;
            }
            if let Some(&to) = aliases.get(else_val) {
                *else_val = to;
            }
        }
        SIRInstruction::Slice(_, src, _, _) => {
            if let Some(&to) = aliases.get(src) {
                *src = to;
            }
        }
        SIRInstruction::RuntimeEvent { args, .. }
        | SIRInstruction::CombCaptureEvent { args, .. } => {
            for arg in args {
                if let Some(&to) = aliases.get(arg) {
                    *arg = to;
                }
            }
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            if let Some(&to) = aliases.get(old) {
                *old = to;
            }
            if let Some(&to) = aliases.get(new) {
                *new = to;
            }
        }
    }
}

/// Remove instructions whose defined register is never used.
fn dead_code_eliminate(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    // Iterate until no more changes (dead chains)
    loop {
        let used = collect_all_used_registers(eu);

        let mut changed = false;
        for block in eu.blocks.values_mut() {
            let before = block.instructions.len();
            block.instructions.retain(|inst| {
                if let Some(dst) = def_reg(inst) {
                    // Keep if the register is used somewhere
                    used.contains(&dst)
                } else {
                    // Store/Commit — always keep
                    true
                }
            });
            if block.instructions.len() != before {
                changed = true;
            }
        }

        if !changed {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{InstanceId, STABLE_REGION};
    use veryl_analyzer::ir::VarId;

    fn address() -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr {
            region: STABLE_REGION,
            instance_id: InstanceId(0),
            var_id: VarId::default(),
        }
    }

    fn bit(width: usize) -> RegisterType {
        RegisterType::Bit {
            width,
            signed: false,
        }
    }

    #[test]
    fn does_not_forward_a_truncated_store_as_a_wider_source_register() {
        let addr = address();
        let mut instructions = vec![
            SIRInstruction::Store(
                addr,
                SIROffset::Static(0),
                32,
                RegisterId(0),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 32),
            SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Eq, RegisterId(2)),
        ];
        let register_map = [
            (RegisterId(0), bit(64)),
            (RegisterId(1), bit(32)),
            (RegisterId(2), bit(32)),
            (RegisterId(3), bit(1)),
        ]
        .into_iter()
        .collect();

        forward_and_simplify(&mut instructions, &register_map, false);

        assert!(matches!(
            instructions[2],
            SIRInstruction::Binary(_, RegisterId(1), BinaryOp::Eq, RegisterId(2))
        ));
    }

    #[test]
    fn forwards_a_store_when_source_and_load_types_match() {
        let addr = address();
        let mut instructions = vec![
            SIRInstruction::Store(
                addr,
                SIROffset::Static(0),
                32,
                RegisterId(0),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 32),
            SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Eq, RegisterId(2)),
        ];
        let register_map = [
            (RegisterId(0), bit(32)),
            (RegisterId(1), bit(32)),
            (RegisterId(2), bit(32)),
            (RegisterId(3), bit(1)),
        ]
        .into_iter()
        .collect();

        forward_and_simplify(&mut instructions, &register_map, false);

        assert!(matches!(
            instructions[2],
            SIRInstruction::Binary(_, RegisterId(0), BinaryOp::Eq, RegisterId(2))
        ));
    }

    #[test]
    fn forwards_unsigned_bit_to_logic_in_two_state_mode() {
        let addr = address();
        let mut instructions = vec![
            SIRInstruction::Store(
                addr,
                SIROffset::Static(0),
                32,
                RegisterId(0),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 32),
            SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Eq, RegisterId(2)),
        ];
        let register_map = [
            (RegisterId(0), bit(32)),
            (RegisterId(1), RegisterType::Logic { width: 32 }),
            (RegisterId(2), RegisterType::Logic { width: 32 }),
            (RegisterId(3), bit(1)),
        ]
        .into_iter()
        .collect();

        forward_and_simplify(&mut instructions, &register_map, false);

        assert!(matches!(
            instructions[2],
            SIRInstruction::Binary(_, RegisterId(0), BinaryOp::Eq, RegisterId(2))
        ));
    }

    #[test]
    fn preserves_bit_to_logic_round_trip_in_four_state_mode() {
        let addr = address();
        let mut instructions = vec![
            SIRInstruction::Store(
                addr,
                SIROffset::Static(0),
                32,
                RegisterId(0),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 32),
            SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Eq, RegisterId(2)),
        ];
        let register_map = [
            (RegisterId(0), bit(32)),
            (RegisterId(1), RegisterType::Logic { width: 32 }),
            (RegisterId(2), RegisterType::Logic { width: 32 }),
            (RegisterId(3), bit(1)),
        ]
        .into_iter()
        .collect();

        forward_and_simplify(&mut instructions, &register_map, true);

        assert!(matches!(
            instructions[2],
            SIRInstruction::Binary(_, RegisterId(1), BinaryOp::Eq, RegisterId(2))
        ));
    }

    #[test]
    fn folds_one_bit_reductions_in_four_state_mode() {
        for operation in [UnaryOp::And, UnaryOp::Or, UnaryOp::Xor] {
            let mut instructions = vec![
                SIRInstruction::Unary(RegisterId(1), operation, RegisterId(0)),
                SIRInstruction::Unary(RegisterId(2), UnaryOp::ToTwoState, RegisterId(1)),
            ];
            let register_map = [
                (RegisterId(0), RegisterType::Logic { width: 1 }),
                (RegisterId(1), RegisterType::Logic { width: 1 }),
                (RegisterId(2), bit(1)),
            ]
            .into_iter()
            .collect();

            forward_and_simplify(&mut instructions, &register_map, true);

            assert_eq!(
                instructions[1],
                SIRInstruction::Unary(RegisterId(2), UnaryOp::ToTwoState, RegisterId(0),)
            );
        }
    }

    #[test]
    fn keeps_wide_reductions_and_signed_one_bit_sources() {
        let mut instructions = vec![
            SIRInstruction::Unary(RegisterId(1), UnaryOp::Or, RegisterId(0)),
            SIRInstruction::Unary(RegisterId(3), UnaryOp::Or, RegisterId(2)),
            SIRInstruction::Binary(RegisterId(5), RegisterId(1), BinaryOp::Or, RegisterId(3)),
        ];
        let register_map = [
            (RegisterId(0), RegisterType::Logic { width: 2 }),
            (RegisterId(1), RegisterType::Logic { width: 1 }),
            (
                RegisterId(2),
                RegisterType::Bit {
                    width: 1,
                    signed: true,
                },
            ),
            (RegisterId(3), bit(1)),
            (RegisterId(5), bit(1)),
        ]
        .into_iter()
        .collect();

        forward_and_simplify(&mut instructions, &register_map, false);

        assert_eq!(
            instructions[0],
            SIRInstruction::Unary(RegisterId(1), UnaryOp::Or, RegisterId(0))
        );
        assert_eq!(
            instructions[1],
            SIRInstruction::Unary(RegisterId(3), UnaryOp::Or, RegisterId(2))
        );
    }

    #[test]
    fn does_not_forward_a_signed_bit_as_an_unsigned_logic_value() {
        let addr = address();
        let mut instructions = vec![
            SIRInstruction::Store(
                addr,
                SIROffset::Static(0),
                32,
                RegisterId(0),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(0), 32),
            SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Eq, RegisterId(2)),
        ];
        let register_map = [
            (
                RegisterId(0),
                RegisterType::Bit {
                    width: 32,
                    signed: true,
                },
            ),
            (RegisterId(1), RegisterType::Logic { width: 32 }),
            (RegisterId(2), RegisterType::Logic { width: 32 }),
            (RegisterId(3), bit(1)),
        ]
        .into_iter()
        .collect();

        forward_and_simplify(&mut instructions, &register_map, false);

        assert!(matches!(
            instructions[2],
            SIRInstruction::Binary(_, RegisterId(1), BinaryOp::Eq, RegisterId(2))
        ));
    }

    #[test]
    fn forwards_a_contained_load_as_a_slice() {
        let addr = address();
        let mut instructions = vec![
            SIRInstruction::Store(
                addr,
                SIROffset::Static(0),
                64,
                RegisterId(0),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Load(RegisterId(1), addr, SIROffset::Static(16), 16),
            SIRInstruction::Binary(RegisterId(3), RegisterId(1), BinaryOp::Eq, RegisterId(2)),
        ];
        let register_map = [
            (RegisterId(0), bit(64)),
            (RegisterId(1), bit(16)),
            (RegisterId(2), bit(16)),
            (RegisterId(3), bit(1)),
        ]
        .into_iter()
        .collect();

        forward_and_simplify(&mut instructions, &register_map, false);

        assert_eq!(
            instructions[1],
            SIRInstruction::Slice(RegisterId(1), RegisterId(0), 16, 16)
        );
    }

    #[test]
    fn overlapping_store_invalidates_a_containing_definition() {
        let addr = address();
        let mut instructions = vec![
            SIRInstruction::Store(
                addr,
                SIROffset::Static(0),
                64,
                RegisterId(0),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Store(
                addr,
                SIROffset::Static(8),
                8,
                RegisterId(1),
                Vec::new(),
                Vec::new(),
            ),
            SIRInstruction::Load(RegisterId(2), addr, SIROffset::Static(16), 16),
        ];
        let register_map = [
            (RegisterId(0), bit(64)),
            (RegisterId(1), bit(8)),
            (RegisterId(2), bit(16)),
        ]
        .into_iter()
        .collect();

        forward_and_simplify(&mut instructions, &register_map, false);

        assert!(matches!(instructions[2], SIRInstruction::Load(..)));
    }
}
