use crate::HashMap;
use crate::HashSet;
use crate::ir::*;

fn next_register_id(
    register_map: &mut HashMap<RegisterId, RegisterType>,
    counter: &mut usize,
) -> RegisterId {
    *counter += 1;
    while register_map.contains_key(&RegisterId(*counter)) {
        *counter += 1;
    }
    RegisterId(*counter)
}

/// Append the canonical four-state-to-control conversion required by SIR
/// `Branch`, returning an unsigned one-bit condition register.
pub(in crate::optimizer) fn normalize_branch_condition<A>(
    register_map: &mut HashMap<RegisterId, RegisterType>,
    instructions: &mut Vec<SIRInstruction<A>>,
    cond: RegisterId,
    reg_counter: &mut usize,
) -> RegisterId {
    let cond_type = register_map[&cond].clone();
    if matches!(
        cond_type,
        RegisterType::Bit {
            width: 1,
            signed: false
        }
    ) {
        return cond;
    }

    let truth = if cond_type.width() == 1 {
        cond
    } else {
        let truth = next_register_id(register_map, reg_counter);
        register_map.insert(truth, RegisterType::Logic { width: 1 });
        instructions.push(SIRInstruction::Unary(truth, UnaryOp::Or, cond));
        truth
    };
    let normalized = next_register_id(register_map, reg_counter);
    register_map.insert(
        normalized,
        RegisterType::Bit {
            width: 1,
            signed: false,
        },
    );
    instructions.push(SIRInstruction::Unary(
        normalized,
        UnaryOp::ToTwoState,
        truth,
    ));
    normalized
}

/// Returns `Some(RegisterId)` for instructions that define a register.
pub(in crate::optimizer) fn def_reg<A>(inst: &SIRInstruction<A>) -> Option<RegisterId> {
    inst.defined_register()
}

/// Whether an instruction can be removed because nothing uses the register it
/// defines and it has no side effect.
pub(in crate::optimizer) fn is_unused_definition<A>(
    inst: &SIRInstruction<A>,
    used: &HashSet<RegisterId>,
) -> bool {
    !inst.has_side_effects() && def_reg(inst).is_some_and(|register| !used.contains(&register))
}

/// Try to extract a u64 value from a SIRValue that represents a 2-state constant.
pub(in crate::optimizer) fn sir_value_to_u64(val: &SIRValue) -> Option<u64> {
    if !val.mask.to_u64_digits().is_empty() {
        return None; // 4-state value
    }
    let digits = val.payload.to_u64_digits();
    match digits.len() {
        0 => Some(0),
        1 => Some(digits[0]),
        _ => None,
    }
}

/// Resolve transitive aliases: if A→B and B→C, produce A→C.
pub(in crate::optimizer) fn resolve_transitive_aliases(
    aliases: &HashMap<RegisterId, RegisterId>,
) -> HashMap<RegisterId, RegisterId> {
    let mut resolved = HashMap::default();
    for (&from, &to) in aliases {
        let mut target = to;
        while let Some(&next) = aliases.get(&target) {
            if next == target {
                break;
            }
            target = next;
        }
        resolved.insert(from, target);
    }
    resolved
}

/// Collect all registers that are used (read) anywhere in an execution unit,
/// including instruction operands and terminators.
pub(in crate::optimizer) fn collect_all_used_registers(
    eu: &ExecutionUnit<RegionedAbsoluteAddr>,
) -> HashSet<RegisterId> {
    let mut used = HashSet::default();
    for block in eu.blocks.values() {
        for inst in &block.instructions {
            collect_used_regs_into(inst, &mut used);
        }
        collect_terminator_used_regs(&block.terminator, &mut used);
    }
    used
}

fn collect_used_regs_into(
    inst: &SIRInstruction<RegionedAbsoluteAddr>,
    out: &mut HashSet<RegisterId>,
) {
    inst.for_each_use(|register| {
        out.insert(register);
    });
}

fn collect_terminator_used_regs(term: &SIRTerminator, out: &mut HashSet<RegisterId>) {
    match term {
        SIRTerminator::Jump(_, args) => {
            out.extend(args.iter().copied());
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            out.insert(*cond);
            out.extend(true_block.1.iter().copied());
            out.extend(false_block.1.iter().copied());
        }
        SIRTerminator::Switch { selector, .. } => {
            out.insert(*selector);
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

pub(in crate::optimizer) fn replace_reg_in_terminator(
    term: &mut SIRTerminator,
    from: RegisterId,
    to: RegisterId,
) {
    match term {
        SIRTerminator::Jump(_, args) => {
            for arg in args {
                if *arg == from {
                    *arg = to;
                }
            }
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            if *cond == from {
                *cond = to;
            }
            for arg in &mut true_block.1 {
                if *arg == from {
                    *arg = to;
                }
            }
            for arg in &mut false_block.1 {
                if *arg == from {
                    *arg = to;
                }
            }
        }
        SIRTerminator::Switch { selector, .. } => {
            if *selector == from {
                *selector = to;
            }
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

pub(in crate::optimizer) fn hoist_common_branch_loads(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
) {
    let mut reg_counter: usize = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);

    let Ok(cfg) = crate::ir::cfg::SirCfg::analyze(eu) else {
        return;
    };
    #[derive(Clone)]
    struct Candidate {
        pred: BlockId,
        true_block: BlockId,
        false_block: BlockId,
        offset: usize,
        bits: usize,
        dst_true: RegisterId,
        dst_false: RegisterId,
        addr: RegionedAbsoluteAddr,
        result_type: RegisterType,
    }

    loop {
        let mut candidates = Vec::new();

        let block_ids: Vec<_> = eu.blocks.keys().copied().collect();
        for bid in block_ids {
            let Some(block) = eu.blocks.get(&bid) else {
                continue;
            };

            let SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } = &block.terminator
            else {
                continue;
            };

            // A single new definition in the predecessor must dominate every
            // use formerly reached through either successor's load.
            if true_block.0 == false_block.0
                || true_block.0 == bid
                || false_block.0 == bid
                || [true_block.0, false_block.0].iter().any(|successor| {
                    *successor == eu.entry_block_id
                        || cfg.predecessors[cfg.index[successor]].as_slice() != [cfg.index[&bid]]
                })
            {
                continue;
            }
            let Some(t_block) = eu.blocks.get(&true_block.0) else {
                continue;
            };
            let Some(f_block) = eu.blocks.get(&false_block.0) else {
                continue;
            };

            let Some(SIRInstruction::Load(dst_t, addr_t, SIROffset::Static(off_t), bits_t)) =
                t_block.instructions.first()
            else {
                continue;
            };
            let Some(SIRInstruction::Load(dst_f, addr_f, SIROffset::Static(off_f), bits_f)) =
                f_block.instructions.first()
            else {
                continue;
            };

            if addr_t == addr_f
                && off_t == off_f
                && bits_t == bits_f
                && eu.register_map.get(dst_t) == eu.register_map.get(dst_f)
            {
                candidates.push(Candidate {
                    pred: bid,
                    true_block: true_block.0,
                    false_block: false_block.0,
                    offset: *off_t,
                    bits: *bits_t,
                    dst_true: *dst_t,
                    dst_false: *dst_f,
                    addr: *addr_t,
                    result_type: eu.register_map[dst_t].clone(),
                });
            }
        }

        if candidates.is_empty() {
            break;
        }

        // Batch: collect all replacements, then apply once
        let mut replacement_map: HashMap<RegisterId, RegisterId> = HashMap::default();
        let mut changed = false;

        for c in candidates {
            let can_apply = if let (Some(t_block), Some(f_block)) =
                (eu.blocks.get(&c.true_block), eu.blocks.get(&c.false_block))
            {
                let t_ok = matches!(
                    t_block.instructions.first(),
                    Some(SIRInstruction::Load(dst, addr, SIROffset::Static(off), bits))
                        if *dst == c.dst_true && *addr == c.addr && *off == c.offset && *bits == c.bits
                );
                let f_ok = matches!(
                    f_block.instructions.first(),
                    Some(SIRInstruction::Load(dst, addr, SIROffset::Static(off), bits))
                        if *dst == c.dst_false && *addr == c.addr && *off == c.offset && *bits == c.bits
                );
                t_ok && f_ok
            } else {
                false
            };

            if !can_apply {
                continue;
            }

            let existing_reg = eu.blocks.get(&c.pred).and_then(|pred_block| {
                for inst in pred_block.instructions.iter().rev() {
                    if inst.is_host_interaction() {
                        return None;
                    }
                    if let Some(write) = inst.memory_write()
                        && *write.addr == c.addr
                        && write.offset.constant_bit_offset().is_none_or(|offset| {
                            offset < c.offset.saturating_add(c.bits)
                                && c.offset < offset.saturating_add(write.width)
                        })
                    {
                        return None;
                    }
                    if let SIRInstruction::Load(dst, addr, SIROffset::Static(off), bits) = inst
                        && *addr == c.addr
                        && *off == c.offset
                        && *bits == c.bits
                        && eu.register_map.get(dst) == Some(&c.result_type)
                    {
                        return Some(*dst);
                    }
                }
                None
            });

            let hoisted_reg = existing_reg.unwrap_or_else(|| {
                let new_reg = next_register_id(&mut eu.register_map, &mut reg_counter);
                eu.register_map.insert(new_reg, c.result_type.clone());

                if let Some(pred_block) = eu.blocks.get_mut(&c.pred) {
                    pred_block.instructions.push(SIRInstruction::Load(
                        new_reg,
                        c.addr,
                        SIROffset::Static(c.offset),
                        c.bits,
                    ));
                }
                new_reg
            });

            if let Some(t_block) = eu.blocks.get_mut(&c.true_block) {
                t_block.instructions.remove(0);
            }
            if let Some(f_block) = eu.blocks.get_mut(&c.false_block) {
                f_block.instructions.remove(0);
            }

            replacement_map.insert(c.dst_true, hoisted_reg);
            replacement_map.insert(c.dst_false, hoisted_reg);
            changed = true;
        }

        if !changed {
            break;
        }

        // Resolve transitive replacements
        let mut final_map = HashMap::default();
        for &from in replacement_map.keys() {
            let mut to = replacement_map[&from];
            let mut depth = 0;
            while let Some(&next_to) = replacement_map.get(&to) {
                if next_to == to || depth > replacement_map.len() {
                    break;
                }
                to = next_to;
                depth += 1;
            }
            final_map.insert(from, to);
        }

        // Batch apply all replacements in a single pass over all blocks
        for block in eu.blocks.values_mut() {
            for p in &mut block.params {
                if let Some(&to) = final_map.get(p) {
                    *p = to;
                }
            }
            for inst in &mut block.instructions {
                batch_replace_in_inst(inst, &final_map);
            }
            batch_replace_in_terminator(&mut block.terminator, &final_map);
        }
    }
}

pub(in crate::optimizer) fn batch_replace_in_inst(
    inst: &mut SIRInstruction<RegionedAbsoluteAddr>,
    map: &HashMap<RegisterId, RegisterId>,
) {
    inst.for_each_use_mut(|register| {
        if let Some(&replacement) = map.get(register) {
            *register = replacement;
        }
    });
}

pub(in crate::optimizer) fn batch_replace_in_terminator(
    term: &mut SIRTerminator,
    map: &HashMap<RegisterId, RegisterId>,
) {
    match term {
        SIRTerminator::Jump(_, args) => {
            for arg in args {
                if let Some(&to) = map.get(arg) {
                    *arg = to;
                }
            }
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            if let Some(&to) = map.get(cond) {
                *cond = to;
            }
            for arg in &mut true_block.1 {
                if let Some(&to) = map.get(arg) {
                    *arg = to;
                }
            }
            for arg in &mut false_block.1 {
                if let Some(&to) = map.get(arg) {
                    *arg = to;
                }
            }
        }
        SIRTerminator::Switch { selector, .. } => {
            if let Some(&to) = map.get(selector) {
                *selector = to;
            }
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

#[cfg(test)]
mod hoist_tests {
    use super::*;
    use celox_design::{InstanceId, StateObjectId};
    fn fixture(
        old_type: RegisterType,
        branch_type: RegisterType,
    ) -> ExecutionUnit<RegionedAbsoluteAddr> {
        let addr = RegionedAbsoluteAddr::from_absolute_addr(
            STABLE_REGION,
            AbsoluteAddr {
                instance_id: InstanceId(0),
                var_id: StateObjectId::default(),
            },
        );
        let bit = |width| RegisterType::Bit {
            width,
            signed: false,
        };
        ExecutionUnit {
            entry_block_id: BlockId(0),
            register_map: [
                (RegisterId(0), old_type),
                (RegisterId(1), bit(64)),
                (RegisterId(2), bit(1)),
                (RegisterId(3), branch_type.clone()),
                (RegisterId(4), branch_type),
            ]
            .into_iter()
            .collect(),
            blocks: [
                (
                    BlockId(0),
                    BasicBlock {
                        id: BlockId(0),
                        params: vec![],
                        instructions: vec![
                            SIRInstruction::Load(RegisterId(0), addr, SIROffset::Static(0), 64),
                            SIRInstruction::Imm(RegisterId(1), SIRValue::new(1.5f64.to_bits())),
                            SIRInstruction::Store(
                                addr,
                                SIROffset::Static(0),
                                64,
                                RegisterId(1),
                                vec![],
                                vec![],
                            ),
                            SIRInstruction::Imm(RegisterId(2), SIRValue::new(1u8)),
                        ],
                        terminator: SIRTerminator::Branch {
                            cond: RegisterId(2),
                            true_block: (BlockId(1), vec![]),
                            false_block: (BlockId(2), vec![]),
                        },
                    },
                ),
                (
                    BlockId(1),
                    BasicBlock {
                        id: BlockId(1),
                        params: vec![],
                        instructions: vec![
                            SIRInstruction::Load(RegisterId(3), addr, SIROffset::Static(0), 64),
                            SIRInstruction::RuntimeEvent {
                                site_id: 0,
                                args: vec![RegisterId(3)],
                            },
                        ],
                        terminator: SIRTerminator::Return,
                    },
                ),
                (
                    BlockId(2),
                    BasicBlock {
                        id: BlockId(2),
                        params: vec![],
                        instructions: vec![
                            SIRInstruction::Load(RegisterId(4), addr, SIROffset::Static(0), 64),
                            SIRInstruction::RuntimeEvent {
                                site_id: 0,
                                args: vec![RegisterId(4)],
                            },
                        ],
                        terminator: SIRTerminator::Return,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        }
    }
    #[test]
    fn hoisting_preserves_read_after_write_and_bit_logic_result_types() {
        for old_type in [
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
            RegisterType::Logic { width: 64 },
        ] {
            for branch_type in [
                RegisterType::Bit {
                    width: 64,
                    signed: false,
                },
                RegisterType::Logic { width: 64 },
            ] {
                let mut unit = fixture(old_type.clone(), branch_type.clone());
                unit.verify();
                hoist_common_branch_loads(&mut unit);
                unit.verify();
                let pred = &unit.blocks[&BlockId(0)];
                let store = pred
                    .instructions
                    .iter()
                    .position(|inst| matches!(inst, SIRInstruction::Store(..)))
                    .unwrap();
                let SIRInstruction::Load(new, _, _, _) = pred.instructions.last().unwrap() else {
                    panic!("fresh load must follow the write")
                };
                assert!(store < pred.instructions.len() - 1);
                assert_ne!(*new, RegisterId(0));
                assert_eq!(unit.register_map[new], branch_type);
                for bid in [BlockId(1), BlockId(2)] {
                    assert!(
                        matches!(&unit.blocks[&bid].instructions[0],SIRInstruction::RuntimeEvent{args,..}if args==&vec![*new])
                    );
                }
            }
        }
    }
    #[test]
    fn mismatched_branch_load_types_are_not_merged() {
        let mut unit = fixture(
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
            RegisterType::Logic { width: 64 },
        );
        unit.register_map.insert(
            RegisterId(3),
            RegisterType::Bit {
                width: 64,
                signed: false,
            },
        );
        let before = unit.clone();
        hoist_common_branch_loads(&mut unit);
        assert_eq!(unit, before);
    }
    #[test]
    fn successor_with_another_predecessor_keeps_its_load() {
        let mut unit = fixture(
            RegisterType::Logic { width: 64 },
            RegisterType::Logic { width: 64 },
        );
        unit.blocks.get_mut(&BlockId(2)).unwrap().terminator =
            SIRTerminator::Jump(BlockId(1), vec![]);
        let before = unit.clone();
        hoist_common_branch_loads(&mut unit);
        unit.verify();
        assert_eq!(unit, before);
    }
}
