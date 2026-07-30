use std::collections::{HashMap, HashSet};

use super::mir::*;

pub fn legalize(func: &mut MFunction) {
    eliminate_trivial_phis(func);
    legalize_variable_shift_counts(func);
}

/// Expand an aggregate whose scalar frontier exceeds the target's simultaneous
/// GPR operand capacity into ordered one-use captures.
///
/// The high-level aggregate keeps its complete variable-arity dependency list
/// through MIR optimization. Only this final machine boundary turns those
/// dependencies into their actual sequential uses, allowing ordinary
/// liveness and spilling to expire each scalar after it is captured.
pub(crate) fn legalize_lane_aggregate_inputs(
    func: &mut MFunction,
    simultaneous_gpr_capacity: usize,
) {
    let aggregate_roots = func
        .blocks
        .iter()
        .flat_map(|block| &block.insts)
        .filter_map(|inst| match inst {
            MInst::LaneAggregate { plan, root, .. } => Some((*plan, *root)),
            _ => None,
        })
        .collect::<HashSet<_>>();
    let input_layouts = aggregate_roots
        .into_iter()
        .filter_map(|identity @ (plan_id, root)| {
            func.lane_aggregate_plan(plan_id)?
                .scalar_input_layout_for_root(usize::from(root))
                .map(|layout| (identity, layout))
        })
        .collect::<HashMap<_, _>>();

    let mut next_input_byte = 0usize;
    for block in &mut func.blocks {
        let original = std::mem::take(&mut block.insts);
        let definition_positions = original
            .iter()
            .enumerate()
            .filter_map(|(index, inst)| inst.def().map(|vreg| (vreg, index)))
            .collect::<HashMap<_, _>>();
        let mut captures_before = vec![Vec::<MInst>::new(); original.len() + 1];
        let mut transformed = Vec::with_capacity(original.len());
        for (instruction_index, mut inst) in original.into_iter().enumerate() {
            if let MInst::LaneAggregate {
                plan,
                root,
                inputs,
                captured_inputs,
                input_bytes,
                input_base_offset,
                ..
            } = &mut inst
            {
                let (layout, layout_bytes) = input_layouts
                    .get(&(*plan, *root))
                    .cloned()
                    .unwrap_or_else(|| {
                        (
                            inputs
                                .iter()
                                .enumerate()
                                .map(|(index, _)| {
                                    (
                                        crate::ir::RegisterId(index),
                                        64,
                                        u32::try_from(index * 8).expect("test input offset"),
                                    )
                                })
                                .collect(),
                            u32::try_from(inputs.len() * 8).expect("test input bytes"),
                        )
                    });
                assert_eq!(
                    inputs.len(),
                    layout.len(),
                    "aggregate input layout mismatch"
                );
                *input_bytes = layout_bytes;
                next_input_byte = next_input_byte
                    .checked_add(31)
                    .expect("lane aggregate scratch offset overflow")
                    & !31;
                *input_base_offset = u32::try_from(next_input_byte)
                    .expect("lane aggregate scratch offset exceeds u32");
                let input_count = inputs.len();
                next_input_byte = next_input_byte
                    .checked_add(usize::try_from(layout_bytes).expect("aggregate input bytes"))
                    .expect("lane aggregate scratch size overflow");

                if input_count > simultaneous_gpr_capacity {
                    let count = u16::try_from(input_count)
                        .expect("lane aggregate scalar frontier exceeds u16 slots");
                    let captured = std::mem::take(inputs);
                    let mut group_start = 0usize;
                    while group_start < captured.len() {
                        let packed_word = layout[group_start].1 <= 16;
                        let capacity = if packed_word { 8 } else { 4 };
                        let stride = if packed_word { 2 } else { 8 };
                        let base = layout[group_start].2;
                        let mut group_end = group_start + 1;
                        while group_end < captured.len()
                            && group_end - group_start < capacity
                            && (layout[group_end].1 <= 16) == packed_word
                            && layout[group_end].2
                                == base
                                    + u32::try_from((group_end - group_start) * stride)
                                        .expect("aggregate input offset")
                        {
                            group_end += 1;
                        }
                        let srcs = &captured[group_start..group_end];
                        let placement = srcs
                            .iter()
                            .filter_map(|src| definition_positions.get(src).copied())
                            .map(|definition| definition + 1)
                            .max()
                            .unwrap_or(0);
                        assert!(
                            placement <= instruction_index,
                            "aggregate input is defined after its use"
                        );
                        captures_before[placement].push(MInst::LaneAggregateInput {
                            base_offset: input_base_offset
                                .checked_add(base)
                                .expect("lane aggregate input offset"),
                            srcs: Uses::from_slice(srcs),
                            packed_word,
                        });
                        group_start = group_end;
                    }
                    *captured_inputs = count;
                }
            }
            transformed.push(inst);
        }
        let capture_count = captures_before.iter().map(Vec::len).sum::<usize>();
        let mut rewritten = Vec::with_capacity(transformed.len() + capture_count);
        for (instruction_index, inst) in transformed.into_iter().enumerate() {
            rewritten.append(&mut captures_before[instruction_index]);
            rewritten.push(inst);
        }
        rewritten.append(captures_before.last_mut().unwrap());
        block.insts = rewritten;
    }
}

/// Make the MIR's non-wrapping shift-count semantics explicit before x86
/// emission. x86 masks variable counts (modulo 64 for a 64-bit operand), while
/// MIR defines logical shifts by counts >= 64 as zero and arithmetic shifts as
/// a sign fill. Every variable shift selects the architectural result only when
/// the unsigned count is below 64; the raw x86 shift is never exposed directly.
pub(crate) fn legalize_variable_shift_counts(func: &mut MFunction) {
    let (blocks, vregs, spill_descs) = (&mut func.blocks, &mut func.vregs, &mut func.spill_descs);

    for block in blocks {
        let legalization_count = block
            .insts
            .iter()
            .filter(|inst| {
                matches!(
                    inst,
                    MInst::Shr { .. } | MInst::Shl { .. } | MInst::Sar { .. }
                )
            })
            .count();
        if legalization_count == 0 {
            continue;
        }

        let mut rewritten = Vec::with_capacity(block.insts.len() + legalization_count * 2);
        for inst in std::mem::take(&mut block.insts) {
            match inst {
                MInst::Shr { dst, lhs, rhs } => {
                    let raw = alloc_shift_temp(vregs, spill_descs, false);
                    let zero = alloc_shift_temp(vregs, spill_descs, true);
                    rewritten.push(MInst::Shr { dst: raw, lhs, rhs });
                    rewritten.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    rewritten.push(MInst::CmpImmSelect {
                        dst,
                        lhs: rhs,
                        imm: 64,
                        kind: CmpKind::LtU,
                        true_val: raw,
                        false_val: zero,
                    });
                }
                MInst::Shl { dst, lhs, rhs } => {
                    let raw = alloc_shift_temp(vregs, spill_descs, false);
                    let zero = alloc_shift_temp(vregs, spill_descs, true);
                    rewritten.push(MInst::Shl { dst: raw, lhs, rhs });
                    rewritten.push(MInst::LoadImm {
                        dst: zero,
                        value: 0,
                    });
                    rewritten.push(MInst::CmpImmSelect {
                        dst,
                        lhs: rhs,
                        imm: 64,
                        kind: CmpKind::LtU,
                        true_val: raw,
                        false_val: zero,
                    });
                }
                MInst::Sar { dst, lhs, rhs } => {
                    let raw = alloc_shift_temp(vregs, spill_descs, false);
                    let sign_fill = alloc_shift_temp(vregs, spill_descs, false);
                    rewritten.push(MInst::Sar { dst: raw, lhs, rhs });
                    rewritten.push(MInst::SarImm {
                        dst: sign_fill,
                        src: lhs,
                        imm: 63,
                    });
                    rewritten.push(MInst::CmpImmSelect {
                        dst,
                        lhs: rhs,
                        imm: 64,
                        kind: CmpKind::LtU,
                        true_val: raw,
                        false_val: sign_fill,
                    });
                }
                inst => rewritten.push(inst),
            }
        }
        block.insts = rewritten;
    }
}

fn alloc_shift_temp(
    vregs: &mut VRegAllocator,
    spill_descs: &mut Vec<SpillDesc>,
    rematerialize_zero: bool,
) -> VReg {
    let vreg = vregs.alloc();
    debug_assert_eq!(spill_descs.len(), vreg.0 as usize);
    spill_descs.push(if rematerialize_zero {
        SpillDesc::remat(0)
    } else {
        SpillDesc::transient()
    });
    vreg
}

fn eliminate_trivial_phis(func: &mut MFunction) {
    let mut aliases: HashMap<VReg, VReg> = HashMap::new();

    for block in &func.blocks {
        for phi in &block.phis {
            if phi.sources.is_empty() {
                continue;
            }
            let mut unique_src = None;
            let mut trivial = true;
            for (_, src) in &phi.sources {
                match unique_src {
                    None => unique_src = Some(*src),
                    Some(existing) if existing == *src => {}
                    Some(_) => {
                        trivial = false;
                        break;
                    }
                }
            }
            if trivial {
                if let Some(src) = unique_src {
                    if src != phi.dst {
                        aliases.insert(phi.dst, src);
                    }
                }
            }
        }
    }

    if aliases.is_empty() {
        return;
    }

    let mut resolved: HashMap<VReg, VReg> = HashMap::with_capacity(aliases.len());
    for (&dst, &src) in &aliases {
        let mut target = src;
        let mut seen = HashSet::from([dst]);
        let mut cyclic = false;
        while let Some(&next) = aliases.get(&target) {
            if !seen.insert(target) || !seen.insert(next) {
                cyclic = true;
                break;
            }
            target = next;
        }
        if !cyclic {
            resolved.insert(dst, target);
        }
    }
    aliases = resolved;

    for block in &mut func.blocks {
        for inst in &mut block.insts {
            rewrite_uses(inst, &aliases);
        }
        for phi in &mut block.phis {
            for (_, src) in &mut phi.sources {
                if let Some(&alias) = aliases.get(src) {
                    *src = alias;
                }
            }
        }
        block
            .phis
            .retain(|phi| !matches!(aliases.get(&phi.dst), Some(src) if *src != phi.dst));
    }
}

fn rewrite_uses(inst: &mut MInst, aliases: &HashMap<VReg, VReg>) {
    let current_uses = inst.uses();
    for u in current_uses {
        if let Some(&target) = aliases.get(&u) {
            inst.rewrite_use(u, target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_lane_aggregate_frontier_becomes_bounded_vector_captures() {
        let mut vregs = VRegAllocator::new();
        let inputs = (0..17).map(|_| vregs.alloc()).collect::<Vec<_>>();
        let dst = vregs.alloc();
        let mut func = MFunction::new(
            vregs,
            vec![SpillDesc::transient(); inputs.len().checked_add(1).unwrap()],
        );
        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LaneAggregate {
            dst,
            plan: LaneAggregatePlanId(0),
            root: 0,
            source_block: crate::ir::BlockId(0),
            inputs: inputs.clone(),
            captured_inputs: 0,
            input_bytes: 0,
            input_base_offset: 0,
            read_ranges: Vec::new(),
            write_ranges: Vec::new(),
        });
        block.push(MInst::Return);
        func.push_block(block);

        legalize_lane_aggregate_inputs(&mut func, 15);

        assert_eq!(func.blocks[0].insts.len(), 7);
        for (group, inst) in func.blocks[0].insts[..5].iter().enumerate() {
            let start = group * 4;
            let end = (start + 4).min(inputs.len());
            assert!(matches!(
                inst,
                MInst::LaneAggregateInput {
                    base_offset,
                    srcs,
                    packed_word: false,
                } if usize::try_from(*base_offset).unwrap() == start * 8
                    && srcs.as_slice() == &inputs[start..end]
            ));
            assert_eq!(inst.uses().as_slice(), &inputs[start..end]);
        }
        assert!(matches!(
            &func.blocks[0].insts[5],
            MInst::LaneAggregate {
                inputs: actual,
                captured_inputs: 17,
                input_bytes: 136,
                ..
            } if actual.is_empty()
        ));
    }

    #[test]
    fn eliminates_single_source_phi() {
        let mut vregs = VRegAllocator::new();
        let src = vregs.alloc();
        let dst = vregs.alloc();
        let out = vregs.alloc();
        let spill_descs = vec![
            SpillDesc::transient(),
            SpillDesc::transient(),
            SpillDesc::transient(),
        ];
        let mut func = MFunction::new(vregs, spill_descs);

        let mut block = MBlock::new(BlockId(0));
        block.phis.push(PhiNode {
            dst,
            sources: vec![(BlockId(1), src)],
        });
        block.push(MInst::Mov { dst: out, src: dst });
        block.push(MInst::Return);
        func.push_block(block);

        legalize(&mut func);

        assert!(func.blocks[0].phis.is_empty());
        assert!(matches!(
            func.blocks[0].insts[0],
            MInst::Mov { dst: d, src: s } if d == out && s == src
        ));
    }

    #[test]
    fn leaves_trivial_phi_cycles_intact() {
        let mut vregs = VRegAllocator::new();
        let v0 = vregs.alloc();
        let v1 = vregs.alloc();
        let spill_descs = vec![SpillDesc::transient(), SpillDesc::transient()];
        let mut func = MFunction::new(vregs, spill_descs);

        let mut block = MBlock::new(BlockId(0));
        block.phis.push(PhiNode {
            dst: v0,
            sources: vec![(BlockId(0), v1)],
        });
        block.phis.push(PhiNode {
            dst: v1,
            sources: vec![(BlockId(0), v0)],
        });
        block.push(MInst::Return);
        func.push_block(block);

        legalize(&mut func);

        assert_eq!(func.blocks[0].phis.len(), 2);
        assert_eq!(func.blocks[0].phis[0].sources[0].1, v1);
        assert_eq!(func.blocks[0].phis[1].sources[0].1, v0);
    }

    #[test]
    fn large_variable_shift_counts_are_made_explicit() {
        let mut vregs = VRegAllocator::new();
        let lhs = vregs.alloc();
        let count = vregs.alloc();
        let shl = vregs.alloc();
        let shr = vregs.alloc();
        let sar = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 5]);

        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm {
            dst: lhs,
            value: u64::MAX,
        });
        block.push(MInst::LoadImm {
            dst: count,
            value: 64,
        });
        block.push(MInst::Shl {
            dst: shl,
            lhs,
            rhs: count,
        });
        block.push(MInst::Shr {
            dst: shr,
            lhs,
            rhs: count,
        });
        block.push(MInst::Sar {
            dst: sar,
            lhs,
            rhs: count,
        });
        block.push(MInst::Return);
        func.push_block(block);

        legalize_variable_shift_counts(&mut func);

        assert_eq!(func.vregs.count(), 11);
        assert_eq!(func.spill_descs.len(), 11);
        assert_eq!(
            func.blocks[0]
                .insts
                .iter()
                .filter(|inst| matches!(inst, MInst::CmpImmSelect { imm: 64, .. }))
                .count(),
            3
        );
        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::SarImm {
                src,
                imm: 63,
                ..
            } if *src == lhs
        )));
        func.verify_result().unwrap();
    }

    #[test]
    fn all_variable_shift_counts_are_legalized() {
        let mut vregs = VRegAllocator::new();
        let lhs = vregs.alloc();
        let count = vregs.alloc();
        let dst = vregs.alloc();
        let mut func = MFunction::new(vregs, vec![SpillDesc::transient(); 3]);

        let mut block = MBlock::new(BlockId(0));
        block.push(MInst::LoadImm { dst: lhs, value: 1 });
        block.push(MInst::LoadImm {
            dst: count,
            value: 63,
        });
        block.push(MInst::Shl {
            dst,
            lhs,
            rhs: count,
        });
        block.push(MInst::Return);
        func.push_block(block);

        legalize_variable_shift_counts(&mut func);

        assert_eq!(func.vregs.count(), 5);
        assert_eq!(func.blocks[0].insts.len(), 6);
        assert!(func.blocks[0].insts.iter().any(|inst| matches!(
            inst,
            MInst::CmpImmSelect {
                dst: selected,
                imm: 64,
                ..
            } if *selected == dst
        )));
        func.verify_result().unwrap();
    }
}
