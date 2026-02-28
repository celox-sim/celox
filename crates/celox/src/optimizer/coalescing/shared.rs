use crate::HashMap;
use crate::ir::*;

fn next_register_id(register_map: &HashMap<RegisterId, RegisterType>) -> RegisterId {
    RegisterId(register_map.keys().map(|r| r.0).max().unwrap_or(0) + 1)
}

fn replace_reg_in_instruction<A>(inst: &mut SIRInstruction<A>, from: RegisterId, to: RegisterId) {
    match inst {
        SIRInstruction::Imm(_, _) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            if *lhs == from {
                *lhs = to;
            }
            if *rhs == from {
                *rhs = to;
            }
        }
        SIRInstruction::Unary(_, _, src) => {
            if *src == from {
                *src = to;
            }
        }
        SIRInstruction::Load(_, _, SIROffset::Dynamic(off), _) => {
            if *off == from {
                *off = to;
            }
        }
        SIRInstruction::Load(_, _, SIROffset::Static(_), _) => {}
        SIRInstruction::Store(_, SIROffset::Dynamic(off), _, src, _) => {
            if *off == from {
                *off = to;
            }
            if *src == from {
                *src = to;
            }
        }
        SIRInstruction::Store(_, SIROffset::Static(_), _, src, _) => {
            if *src == from {
                *src = to;
            }
        }
        SIRInstruction::Commit(_, _, SIROffset::Dynamic(off), _, _) => {
            if *off == from {
                *off = to;
            }
        }
        SIRInstruction::Commit(_, _, SIROffset::Static(_), _, _) => {}
        SIRInstruction::Concat(_, args) => {
            for arg in args {
                if *arg == from {
                    *arg = to;
                }
            }
        }
    }
}

pub(super) fn replace_reg_in_terminator(
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
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}

pub(super) fn replace_register_uses(
    eu: &mut ExecutionUnit<RegionedAbsoluteAddr>,
    from: RegisterId,
    to: RegisterId,
) {
    for block in eu.blocks.values_mut() {
        for p in &mut block.params {
            if *p == from {
                *p = to;
            }
        }
        for inst in &mut block.instructions {
            replace_reg_in_instruction(inst, from, to);
        }
        replace_reg_in_terminator(&mut block.terminator, from, to);
    }
}

pub(super) fn hoist_common_branch_loads(eu: &mut ExecutionUnit<RegionedAbsoluteAddr>) {
    #[derive(Clone, Copy)]
    struct Candidate {
        pred: BlockId,
        true_block: BlockId,
        false_block: BlockId,
        offset: usize,
        bits: usize,
        dst_true: RegisterId,
        dst_false: RegisterId,
        addr: RegionedAbsoluteAddr,
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

            if addr_t == addr_f && off_t == off_f && bits_t == bits_f {
                candidates.push(Candidate {
                    pred: bid,
                    true_block: true_block.0,
                    false_block: false_block.0,
                    offset: *off_t,
                    bits: *bits_t,
                    dst_true: *dst_t,
                    dst_false: *dst_f,
                    addr: *addr_t,
                });
            }
        }

        if candidates.is_empty() {
            break;
        }

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

            let hoisted_reg = if let Some(pred_block) = eu.blocks.get(&c.pred) {
                pred_block.instructions.iter().find_map(|inst| match inst {
                    SIRInstruction::Load(dst, addr, SIROffset::Static(off), bits)
                        if *addr == c.addr && *off == c.offset && *bits == c.bits =>
                    {
                        Some(*dst)
                    }
                    _ => None,
                })
            } else {
                None
            }
            .unwrap_or_else(|| {
                let new_reg = next_register_id(&eu.register_map);
                eu.register_map
                    .insert(new_reg, RegisterType::Logic { width: c.bits });

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

            replace_register_uses(eu, c.dst_true, hoisted_reg);
            replace_register_uses(eu, c.dst_false, hoisted_reg);
            changed = true;
        }

        if !changed {
            break;
        }
    }
}
