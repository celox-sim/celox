//! Partial Store-Load Forwarding Pass
//!
//! Replaces full-width Loads that follow narrow Stores to the same variable
//! with a Concat of aligned gap-Loads (unchanged regions) + overlay registers.
//! Gap Loads are split at 64-bit boundaries to keep each Load aligned and cheap
//! in the Cranelift backend (3 CLIF insts vs 7*nc+5 for unaligned).
//!
//! Requires overlap-aware sealing in coalesce_stores to allow coalescing of
//! Stores that aren't read by the gap Loads.

use super::pass_manager::ExecutionUnitPass;
use crate::HashMap;
use crate::PassOptions;
use crate::ir::*;

pub(super) struct PartialForwardPass;

impl ExecutionUnitPass for PartialForwardPass {
    fn name(&self) -> &'static str {
        "partial_store_load_forward"
    }

    fn run(&self, eu: &mut ExecutionUnit<RegionedAbsoluteAddr>, _options: &PassOptions) {
        let mut reg_counter = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);

        for block in eu.blocks.values_mut() {
            partial_forward_block(block, &mut eu.register_map, &mut reg_counter);
        }
    }
}

struct VarState {
    base_reg: RegisterId,
    base_off: usize,
    base_width: usize,
    /// (relative_offset, width, src_reg)
    overlays: Vec<(usize, usize, RegisterId)>,
}

fn partial_forward_block(
    block: &mut BasicBlock<RegionedAbsoluteAddr>,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    reg_counter: &mut usize,
) {
    let mut var_states: HashMap<RegionedAbsoluteAddr, VarState> = HashMap::default();

    let old_instructions = std::mem::take(&mut block.instructions);
    let mut new_instructions: Vec<SIRInstruction<RegionedAbsoluteAddr>> =
        Vec::with_capacity(old_instructions.len());

    for inst in old_instructions {
        match &inst {
            SIRInstruction::Load(dst, addr, SIROffset::Static(off), width) => {
                if let Some(state) = var_states.get(addr) {
                    if *off == state.base_off
                        && *width == state.base_width
                        && !state.overlays.is_empty()
                    {
                        let synth = synthesize_load(*dst, *addr, state, register_map, reg_counter);
                        new_instructions.extend(synth);

                        let state = var_states.get_mut(addr).unwrap();
                        state.base_reg = *dst;
                        state.overlays.clear();
                        continue;
                    }
                }

                var_states.insert(
                    *addr,
                    VarState {
                        base_reg: *dst,
                        base_off: *off,
                        base_width: *width,
                        overlays: Vec::new(),
                    },
                );
                new_instructions.push(inst);
            }
            SIRInstruction::Store(addr, SIROffset::Static(off), width, src, triggers, _)
                if triggers.is_empty() =>
            {
                if let Some(state) = var_states.get_mut(addr) {
                    if *off >= state.base_off && *off + *width <= state.base_off + state.base_width
                    {
                        let rel_off = *off - state.base_off;
                        let store_end = rel_off + *width;
                        let old_overlays = std::mem::take(&mut state.overlays);
                        for (old_off, old_width, old_reg) in old_overlays {
                            let old_end = old_off + old_width;
                            if store_end <= old_off || old_end <= rel_off {
                                state.overlays.push((old_off, old_width, old_reg));
                                continue;
                            }
                            if old_off < rel_off {
                                let preserved_width = rel_off - old_off;
                                let preserved =
                                    alloc_reg(register_map, reg_counter, preserved_width);
                                new_instructions.push(SIRInstruction::Slice(
                                    preserved,
                                    old_reg,
                                    0,
                                    preserved_width,
                                ));
                                state.overlays.push((old_off, preserved_width, preserved));
                            }
                            if store_end < old_end {
                                let preserved_width = old_end - store_end;
                                let preserved =
                                    alloc_reg(register_map, reg_counter, preserved_width);
                                new_instructions.push(SIRInstruction::Slice(
                                    preserved,
                                    old_reg,
                                    store_end - old_off,
                                    preserved_width,
                                ));
                                state.overlays.push((store_end, preserved_width, preserved));
                            }
                        }
                        state.overlays.push((rel_off, *width, *src));
                        new_instructions.push(inst);
                        continue;
                    }
                }
                var_states.remove(addr);
                new_instructions.push(inst);
            }
            SIRInstruction::Store(
                addr,
                SIROffset::Dynamic(_) | SIROffset::Element { .. },
                _,
                _,
                _,
                _,
            ) => {
                var_states.remove(addr);
                new_instructions.push(inst);
            }
            SIRInstruction::Commit(_, dst, _, _, _) => {
                var_states.remove(dst);
                new_instructions.push(inst);
            }
            _ => {
                new_instructions.push(inst);
            }
        }
    }

    block.instructions = new_instructions;
}

/// Emit aligned gap Loads + overlay regs, assembled with Concat.
fn synthesize_load(
    dst: RegisterId,
    addr: RegionedAbsoluteAddr,
    state: &VarState,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    reg_counter: &mut usize,
) -> Vec<SIRInstruction<RegionedAbsoluteAddr>> {
    let mut instructions = Vec::new();

    let mut overlays = state.overlays.clone();
    overlays.sort_by_key(|(off, _, _)| *off);

    let mut concat_args: Vec<RegisterId> = Vec::new(); // LSB-first
    let mut cursor = 0usize;

    for &(rel_off, width, reg) in &overlays {
        if rel_off > cursor {
            emit_gap(
                addr,
                state.base_off,
                state.base_width,
                state.base_reg,
                cursor,
                rel_off - cursor,
                &mut concat_args,
                register_map,
                reg_counter,
                &mut instructions,
            );
        }
        concat_args.push(reg);
        cursor = rel_off + width;
    }

    if cursor < state.base_width {
        emit_gap(
            addr,
            state.base_off,
            state.base_width,
            state.base_reg,
            cursor,
            state.base_width - cursor,
            &mut concat_args,
            register_map,
            reg_counter,
            &mut instructions,
        );
    }

    if concat_args.len() == 1 {
        instructions.push(SIRInstruction::Unary(dst, UnaryOp::Ident, concat_args[0]));
    } else {
        concat_args.reverse(); // MSB-first
        instructions.push(SIRInstruction::Concat(dst, concat_args));
    }

    register_map.insert(
        dst,
        RegisterType::Logic {
            width: state.base_width,
        },
    );

    instructions
}

/// Emit a Slice instruction to extract gap bits from base register.
/// O(1) in the CLIF backend — directly indexes into chunk array.
#[allow(clippy::too_many_arguments)]
fn emit_gap(
    _addr: RegionedAbsoluteAddr,
    _base_off: usize,
    _base_width: usize,
    base_reg: RegisterId,
    gap_rel_start: usize,
    gap_width: usize,
    concat_args: &mut Vec<RegisterId>,
    register_map: &mut HashMap<RegisterId, RegisterType>,
    reg_counter: &mut usize,
    instructions: &mut Vec<SIRInstruction<RegionedAbsoluteAddr>>,
) {
    let reg = alloc_reg(register_map, reg_counter, gap_width);
    instructions.push(SIRInstruction::Slice(
        reg,
        base_reg,
        gap_rel_start,
        gap_width,
    ));
    concat_args.push(reg);
}

fn alloc_reg(
    register_map: &mut HashMap<RegisterId, RegisterType>,
    counter: &mut usize,
    width: usize,
) -> RegisterId {
    *counter += 1;
    while register_map.contains_key(&RegisterId(*counter)) {
        *counter += 1;
    }
    let reg = RegisterId(*counter);
    register_map.insert(reg, RegisterType::Logic { width });
    reg
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{AbsoluteAddr, InstanceId, STABLE_REGION};
    use celox_design::StateObjectId as VarId;

    fn address() -> RegionedAbsoluteAddr {
        RegionedAbsoluteAddr::from_absolute_addr(
            STABLE_REGION,
            AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: VarId::from_raw(0),
            },
        )
    }

    fn logic(width: usize) -> RegisterType {
        RegisterType::Logic { width }
    }

    #[test]
    fn forwards_a_narrow_store_into_a_following_full_load() {
        let base = RegisterId(0);
        let overlay = RegisterId(1);
        let result = RegisterId(2);
        let mut block = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Load(base, address(), SIROffset::Static(0), 8),
                SIRInstruction::Store(
                    address(),
                    SIROffset::Static(2),
                    2,
                    overlay,
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Load(result, address(), SIROffset::Static(0), 8),
            ],
            terminator: SIRTerminator::Return,
        };
        let mut registers = [(base, logic(8)), (overlay, logic(2)), (result, logic(8))]
            .into_iter()
            .collect();
        let mut next = 2;

        partial_forward_block(&mut block, &mut registers, &mut next);

        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|instruction| matches!(instruction, SIRInstruction::Load(..)))
                .count(),
            1
        );
        assert!(matches!(
            block.instructions.last(),
            Some(SIRInstruction::Concat(dst, arguments))
                if *dst == result && arguments.contains(&overlay)
        ));
    }

    #[test]
    fn a_partially_overlapping_store_preserves_both_sides_of_the_old_overlay() {
        let base = RegisterId(0);
        let old = RegisterId(1);
        let new = RegisterId(2);
        let result = RegisterId(3);
        let mut block = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Load(base, address(), SIROffset::Static(0), 8),
                SIRInstruction::Store(
                    address(),
                    SIROffset::Static(2),
                    3,
                    old,
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Store(
                    address(),
                    SIROffset::Static(3),
                    1,
                    new,
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Load(result, address(), SIROffset::Static(0), 8),
            ],
            terminator: SIRTerminator::Return,
        };
        let mut registers = [
            (base, logic(8)),
            (old, logic(3)),
            (new, logic(1)),
            (result, logic(8)),
        ]
        .into_iter()
        .collect();
        let mut next = 3;

        partial_forward_block(&mut block, &mut registers, &mut next);

        let preserved = block
            .instructions
            .iter()
            .filter_map(|instruction| match instruction {
                SIRInstruction::Slice(_, source, offset, width) if *source == old => {
                    Some((*offset, *width))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(preserved, vec![(0, 1), (2, 1)]);
        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|instruction| matches!(instruction, SIRInstruction::Load(..)))
                .count(),
            1
        );
    }

    #[test]
    fn a_dynamic_store_invalidates_the_forwarding_state() {
        let base = RegisterId(0);
        let offset = RegisterId(1);
        let overlay = RegisterId(2);
        let result = RegisterId(3);
        let mut block = BasicBlock {
            id: BlockId(0),
            params: Vec::new(),
            instructions: vec![
                SIRInstruction::Load(base, address(), SIROffset::Static(0), 8),
                SIRInstruction::Store(
                    address(),
                    SIROffset::Dynamic(offset),
                    1,
                    overlay,
                    Vec::new(),
                    Vec::new(),
                ),
                SIRInstruction::Load(result, address(), SIROffset::Static(0), 8),
            ],
            terminator: SIRTerminator::Return,
        };
        let mut registers = [
            (base, logic(8)),
            (offset, logic(3)),
            (overlay, logic(1)),
            (result, logic(8)),
        ]
        .into_iter()
        .collect();
        let mut next = 3;

        partial_forward_block(&mut block, &mut registers, &mut next);

        assert_eq!(
            block
                .instructions
                .iter()
                .filter(|instruction| matches!(instruction, SIRInstruction::Load(..)))
                .count(),
            2
        );
    }
}
