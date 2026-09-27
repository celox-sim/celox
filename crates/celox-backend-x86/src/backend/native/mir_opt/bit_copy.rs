//! Bit partition reconstruction and relocated bit-copy groups.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SelectTerm {
    cond: VReg,
    true_val: VReg,
    false_val: VReg,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LinearBitCopy {
    /// Value at the root of this one OR term before tracing through its
    /// shift/mask chain.
    expression: VReg,
    source: VReg,
    /// `output_bit = source_bit + shift`.
    shift: i16,
    /// Output bits which still copy the corresponding source bit.
    output_mask: u64,
}

const MAX_RECONSTRUCTION_TERMS: usize = 64;
const MAX_RECONSTRUCTION_DEPTH: usize = 64;

/// Collapse an OR tree which partitions one value into bit fields and deposits
/// every field back at its original position.
///
/// SIR struct projection followed by reconstruction commonly lowers to:
///
/// ```text
/// ((src >> 0) & m0) << 0 |
/// ((src >> n) & mn) << n | ...
/// ```
///
/// Ordinary GVN cannot see that the complete tree is just `src & union(mask)`,
/// because every field has a different SSA definition.  Track the affine bit
/// position through only the operations which copy bits (`mov`, logical shifts,
/// and immediate masks).  A rewrite is made only when every nonzero OR term
/// copies bits from one source with `output_bit == source_bit`; relocated bits,
/// constants, and mixed sources are rejected.
pub(super) fn fold_reconstructed_bit_partitions(func: &mut MFunction) {
    for block in &mut func.blocks {
        let definitions = block
            .insts
            .iter()
            .enumerate()
            .filter_map(|(index, instruction)| instruction.def().map(|dst| (dst, index)))
            .collect::<HashMap<_, _>>();
        let mut replacements = Vec::new();

        for (index, instruction) in block.insts.iter().enumerate() {
            let dst = match instruction {
                MInst::Or { dst, .. } | MInst::Or32 { dst, .. } => *dst,
                _ => continue,
            };
            let mut terms = Vec::new();
            if !collect_reconstructed_terms(
                dst,
                u64::MAX,
                &block.insts,
                &definitions,
                &mut terms,
                0,
            ) || terms.len() < 2
            {
                continue;
            }

            let source = terms[0].source;
            if terms
                .iter()
                .any(|term| term.source != source || term.shift != 0)
            {
                continue;
            }
            let mask = terms
                .iter()
                .fold(0u64, |mask, term| mask | term.output_mask);
            let replacement = if mask == u64::MAX {
                MInst::Mov { dst, src: source }
            } else if mask == u64::from(u32::MAX) {
                MInst::Mov32 { dst, src: source }
            } else if let Ok(imm) = u32::try_from(mask) {
                MInst::AndImm32 {
                    dst,
                    src: source,
                    imm,
                }
            } else if and_imm_ok(mask) {
                MInst::AndImm {
                    dst,
                    src: source,
                    imm: mask,
                }
            } else {
                continue;
            };
            replacements.push((index, replacement));
        }

        for (index, replacement) in replacements {
            block.insts[index] = replacement;
        }
    }
}

#[derive(Clone, Copy)]
enum RelocatedBitGroup {
    Existing(VReg),
    Discarded,
    Rebuild {
        source: VReg,
        shift: i16,
        output_mask: u64,
        terms: usize,
        first_expression: VReg,
    },
}

#[derive(Clone)]
struct RelocatedBitPlan {
    instruction: usize,
    destination: VReg,
    word32: bool,
    groups: Vec<RelocatedBitGroup>,
}

/// Coalesce same-source bit-copy terms inside a mixed reconstruction.
///
/// A complete reconstruction can contain fields from several sources:
///
/// ```text
/// opaque
///   | (((source >> 42) & 1) << 1)
///   | (((source >> 43) & 1) << 2)
///   | ...
/// ```
///
/// The complete-tree fold above must reject this because the result is not
/// simply `source & mask`.  The repeated terms nevertheless have one exact
/// affine relation, `output_bit = source_bit + shift`, and can be replaced by
/// one shift and mask.  Only maximal OR roots are rebuilt.  A traced term is
/// grouped only when every bypassed definition is single-use, so shared field
/// projections remain available without duplicating their computation.
pub(super) fn fold_relocated_bit_copy_groups(func: &mut MFunction) {
    // A bypassed projection must be private to the reconstructed root across
    // the whole function.  In particular, phi sources are edge uses and must
    // not disappear merely because this pass scans one block at a time.
    let mut use_counts = HashMap::<VReg, usize>::default();
    for block in &func.blocks {
        for phi in &block.phis {
            for &(_, source) in &phi.sources {
                *use_counts.entry(source).or_default() += 1;
            }
        }
        for instruction in &block.insts {
            for source in instruction.uses() {
                *use_counts.entry(source).or_default() += 1;
            }
        }
    }

    for block_index in 0..func.blocks.len() {
        let plans = {
            let block = &func.blocks[block_index];
            let definitions = block
                .insts
                .iter()
                .enumerate()
                .filter_map(|(index, instruction)| instruction.def().map(|dst| (dst, index)))
                .collect::<HashMap<_, _>>();
            let mut nested_or_inputs = HashSet::<VReg>::default();
            for instruction in &block.insts {
                match instruction {
                    MInst::Or { lhs, rhs, .. } | MInst::Or32 { lhs, rhs, .. } => {
                        nested_or_inputs.insert(*lhs);
                        nested_or_inputs.insert(*rhs);
                    }
                    _ => {}
                }
            }

            let mut plans = Vec::new();
            for (instruction, root) in block.insts.iter().enumerate() {
                let (dst, word32) = match root {
                    MInst::Or { dst, .. } => (*dst, false),
                    MInst::Or32 { dst, .. } => (*dst, true),
                    _ => continue,
                };
                if nested_or_inputs.contains(&dst) {
                    continue;
                }

                let mut terms = Vec::new();
                if !collect_relocated_terms(
                    dst,
                    dst,
                    u64::MAX,
                    &block.insts,
                    &definitions,
                    &use_counts,
                    &mut terms,
                    0,
                ) || terms.len() < 3
                {
                    continue;
                }

                let mut groups = Vec::<RelocatedBitGroup>::new();
                let mut grouped = HashMap::<(VReg, i16), usize>::default();
                let mut shared_terms = Vec::<(usize, (VReg, i16), u64)>::new();
                for term in terms {
                    // A nonzero copied mask normally proves that the affine
                    // displacement remains representable by one machine
                    // shift.  Keep the original expression instead of making
                    // that fact an unchecked release-build precondition.
                    if term.shift.unsigned_abs() >= 64 {
                        groups.push(RelocatedBitGroup::Existing(term.expression));
                        continue;
                    }
                    if !linear_bit_copy_path_is_exclusive(
                        term.expression,
                        term.source,
                        &block.insts,
                        &definitions,
                        &use_counts,
                    ) {
                        shared_terms.push((
                            groups.len(),
                            (term.source, term.shift),
                            term.output_mask,
                        ));
                        groups.push(RelocatedBitGroup::Existing(term.expression));
                        continue;
                    }
                    let key = (term.source, term.shift);
                    if let Some(&group) = grouped.get(&key) {
                        let RelocatedBitGroup::Rebuild {
                            output_mask, terms, ..
                        } = &mut groups[group]
                        else {
                            unreachable!("grouped bit-copy entries are rebuild candidates");
                        };
                        *output_mask |= term.output_mask;
                        *terms += 1;
                    } else {
                        grouped.insert(key, groups.len());
                        groups.push(RelocatedBitGroup::Rebuild {
                            source: term.source,
                            shift: term.shift,
                            output_mask: term.output_mask,
                            terms: 1,
                            first_expression: term.expression,
                        });
                    }
                }

                // Once at least two private terms already pay for a rebuilt
                // shift/mask, including a shared term with the same affine
                // mapping is free.  Its original expression remains for the
                // other use, while this OR root no longer needs the final
                // projection and one extra OR.
                for (existing, key, output_mask) in shared_terms {
                    let Some(&group) = grouped.get(&key) else {
                        continue;
                    };
                    let RelocatedBitGroup::Rebuild {
                        output_mask: grouped_mask,
                        terms,
                        ..
                    } = &mut groups[group]
                    else {
                        unreachable!("grouped bit-copy entries are rebuild candidates");
                    };
                    if *terms >= 2 {
                        *grouped_mask |= output_mask;
                        groups[existing] = RelocatedBitGroup::Discarded;
                    }
                }

                let mut changed = false;
                for group in &mut groups {
                    if let RelocatedBitGroup::Rebuild {
                        terms,
                        first_expression,
                        ..
                    } = *group
                        && terms == 1
                    {
                        *group = RelocatedBitGroup::Existing(first_expression);
                    } else if matches!(
                        group,
                        RelocatedBitGroup::Rebuild { terms, .. } if *terms >= 2
                    ) {
                        changed = true;
                    }
                }
                if changed {
                    plans.push(RelocatedBitPlan {
                        instruction,
                        destination: dst,
                        word32,
                        groups,
                    });
                }
            }
            plans
        };

        for plan in plans.into_iter().rev() {
            let replacement = materialize_relocated_bit_plan(func, &plan);
            func.blocks[block_index]
                .insts
                .splice(plan.instruction..=plan.instruction, replacement);
        }
    }
}

fn collect_relocated_terms(
    root: VReg,
    value: VReg,
    output_mask: u64,
    instructions: &[MInst],
    definitions: &HashMap<VReg, usize>,
    use_counts: &HashMap<VReg, usize>,
    terms: &mut Vec<LinearBitCopy>,
    depth: usize,
) -> bool {
    if depth >= MAX_RECONSTRUCTION_DEPTH || terms.len() >= MAX_RECONSTRUCTION_TERMS {
        return false;
    }
    let Some(&definition) = definitions.get(&value) else {
        terms.push(LinearBitCopy {
            expression: value,
            source: value,
            shift: 0,
            output_mask,
        });
        return true;
    };

    // Flatten only private 64-bit OR nodes.  A nested Or32 is a truncation
    // boundary, not merely an associative OR; retaining it as one opaque term
    // preserves that boundary.  The root Or32 remains safe because the rebuilt
    // chain also ends in Or32.
    if let MInst::Or { lhs, rhs, .. } = instructions[definition]
        && (value == root || use_counts.get(&value).copied() == Some(1))
    {
        return collect_relocated_terms(
            root,
            lhs,
            output_mask,
            instructions,
            definitions,
            use_counts,
            terms,
            depth + 1,
        ) && collect_relocated_terms(
            root,
            rhs,
            output_mask,
            instructions,
            definitions,
            use_counts,
            terms,
            depth + 1,
        );
    }
    if value == root
        && let MInst::Or32 { lhs, rhs, .. } = instructions[definition]
    {
        let output_mask = output_mask & u64::from(u32::MAX);
        return collect_relocated_terms(
            root,
            lhs,
            output_mask,
            instructions,
            definitions,
            use_counts,
            terms,
            depth + 1,
        ) && collect_relocated_terms(
            root,
            rhs,
            output_mask,
            instructions,
            definitions,
            use_counts,
            terms,
            depth + 1,
        );
    }

    let Some(mut term) = trace_linear_bit_copy(value, instructions, definitions, depth + 1) else {
        return false;
    };
    term.expression = value;
    term.output_mask &= output_mask;
    if term.output_mask != 0 {
        terms.push(term);
    }
    true
}

fn linear_bit_copy_path_is_exclusive(
    mut value: VReg,
    source: VReg,
    instructions: &[MInst],
    definitions: &HashMap<VReg, usize>,
    use_counts: &HashMap<VReg, usize>,
) -> bool {
    while value != source {
        if use_counts.get(&value).copied() != Some(1) {
            return false;
        }
        let Some(&definition) = definitions.get(&value) else {
            return false;
        };
        value = match instructions[definition] {
            MInst::Mov { src, .. }
            | MInst::Mov32 { src, .. }
            | MInst::AndImm { src, .. }
            | MInst::AndImm32 { src, .. }
            | MInst::ShrImm { src, .. }
            | MInst::ShlImm { src, .. } => src,
            _ => return false,
        };
    }
    true
}

fn materialize_relocated_bit_plan(func: &mut MFunction, plan: &RelocatedBitPlan) -> Vec<MInst> {
    let mut instructions = Vec::new();
    let mut values = Vec::with_capacity(plan.groups.len());
    for group in &plan.groups {
        match *group {
            RelocatedBitGroup::Existing(value) => values.push(value),
            RelocatedBitGroup::Discarded => {}
            RelocatedBitGroup::Rebuild {
                source,
                shift,
                output_mask,
                ..
            } => {
                let value = materialize_relocated_bit_group(
                    &mut func.vregs,
                    &mut func.spill_descs,
                    &mut instructions,
                    source,
                    shift,
                    output_mask,
                );
                values.push(value);
            }
        }
    }

    let destination = match plan.word32 {
        false => match values.as_slice() {
            [value] => MInst::Mov {
                dst: plan.destination,
                src: *value,
            },
            _ => build_relocated_or_chain(
                &mut func.vregs,
                &mut func.spill_descs,
                &mut instructions,
                &values,
                plan.destination,
                false,
            ),
        },
        true => match values.as_slice() {
            [value] => MInst::Mov32 {
                dst: plan.destination,
                src: *value,
            },
            _ => build_relocated_or_chain(
                &mut func.vregs,
                &mut func.spill_descs,
                &mut instructions,
                &values,
                plan.destination,
                true,
            ),
        },
    };
    instructions.push(destination);
    instructions
}

fn build_relocated_or_chain(
    vregs: &mut VRegAllocator,
    spill_descs: &mut Vec<SpillDesc>,
    instructions: &mut Vec<MInst>,
    values: &[VReg],
    destination: VReg,
    word32: bool,
) -> MInst {
    debug_assert!(values.len() >= 2);
    let mut current = values[0];
    for &value in &values[1..values.len() - 1] {
        let dst = alloc_transient_vreg(vregs, spill_descs);
        instructions.push(MInst::Or {
            dst,
            lhs: current,
            rhs: value,
        });
        current = dst;
    }
    if word32 {
        MInst::Or32 {
            dst: destination,
            lhs: current,
            rhs: values[values.len() - 1],
        }
    } else {
        MInst::Or {
            dst: destination,
            lhs: current,
            rhs: values[values.len() - 1],
        }
    }
}

fn materialize_relocated_bit_group(
    vregs: &mut VRegAllocator,
    spill_descs: &mut Vec<SpillDesc>,
    instructions: &mut Vec<MInst>,
    source: VReg,
    shift: i16,
    output_mask: u64,
) -> VReg {
    debug_assert!(output_mask != 0);
    let magnitude = shift.unsigned_abs();
    debug_assert!(magnitude < 64);
    let shifted_full_mask = match shift.cmp(&0) {
        std::cmp::Ordering::Less => u64::MAX >> magnitude,
        std::cmp::Ordering::Equal => u64::MAX,
        std::cmp::Ordering::Greater => u64::MAX << magnitude,
    };
    let shifted = if shift == 0 {
        source
    } else {
        let dst = alloc_transient_vreg(vregs, spill_descs);
        instructions.push(if shift < 0 {
            MInst::ShrImm {
                dst,
                src: source,
                imm: magnitude as u8,
            }
        } else {
            MInst::ShlImm {
                dst,
                src: source,
                imm: magnitude as u8,
            }
        });
        dst
    };
    if output_mask == shifted_full_mask {
        return shifted;
    }

    let dst = alloc_transient_vreg(vregs, spill_descs);
    if let Ok(mask) = u32::try_from(output_mask) {
        instructions.push(MInst::AndImm32 {
            dst,
            src: shifted,
            imm: mask,
        });
    } else if and_imm_ok(output_mask) {
        instructions.push(MInst::AndImm {
            dst,
            src: shifted,
            imm: output_mask,
        });
    } else {
        let mask = alloc_transient_vreg(vregs, spill_descs);
        spill_descs[mask.0 as usize] = SpillDesc::remat(output_mask);
        instructions.push(MInst::LoadImm {
            dst: mask,
            value: output_mask,
        });
        instructions.push(MInst::And {
            dst,
            lhs: shifted,
            rhs: mask,
        });
    }
    dst
}

fn collect_reconstructed_terms(
    value: VReg,
    output_mask: u64,
    instructions: &[MInst],
    definitions: &HashMap<VReg, usize>,
    terms: &mut Vec<LinearBitCopy>,
    depth: usize,
) -> bool {
    if depth >= MAX_RECONSTRUCTION_DEPTH || terms.len() >= MAX_RECONSTRUCTION_TERMS {
        return false;
    }
    let Some(&definition) = definitions.get(&value) else {
        terms.push(LinearBitCopy {
            expression: value,
            source: value,
            shift: 0,
            output_mask,
        });
        return true;
    };

    match &instructions[definition] {
        MInst::Or { lhs, rhs, .. } => {
            collect_reconstructed_terms(
                *lhs,
                output_mask,
                instructions,
                definitions,
                terms,
                depth + 1,
            ) && collect_reconstructed_terms(
                *rhs,
                output_mask,
                instructions,
                definitions,
                terms,
                depth + 1,
            )
        }
        MInst::Or32 { lhs, rhs, .. } => {
            let output_mask = output_mask & u64::from(u32::MAX);
            collect_reconstructed_terms(
                *lhs,
                output_mask,
                instructions,
                definitions,
                terms,
                depth + 1,
            ) && collect_reconstructed_terms(
                *rhs,
                output_mask,
                instructions,
                definitions,
                terms,
                depth + 1,
            )
        }
        MInst::LoadImm { value: 0, .. } => true,
        _ => {
            let Some(mut term) = trace_linear_bit_copy(value, instructions, definitions, depth + 1)
            else {
                return false;
            };
            term.expression = value;
            term.output_mask &= output_mask;
            if term.output_mask != 0 {
                terms.push(term);
            }
            true
        }
    }
}

fn trace_linear_bit_copy(
    value: VReg,
    instructions: &[MInst],
    definitions: &HashMap<VReg, usize>,
    depth: usize,
) -> Option<LinearBitCopy> {
    if depth >= MAX_RECONSTRUCTION_DEPTH {
        return None;
    }
    let Some(&definition) = definitions.get(&value) else {
        return Some(LinearBitCopy {
            expression: value,
            source: value,
            shift: 0,
            output_mask: u64::MAX,
        });
    };

    let instruction = &instructions[definition];
    let (source, operation) = match instruction {
        MInst::Mov { src, .. } => (*src, LinearCopyOp::Mask(u64::MAX)),
        MInst::Mov32 { src, .. } => (*src, LinearCopyOp::Mask(u64::from(u32::MAX))),
        MInst::AndImm { src, imm, .. } => (*src, LinearCopyOp::Mask(*imm)),
        MInst::AndImm32 { src, imm, .. } => (*src, LinearCopyOp::Mask(u64::from(*imm))),
        MInst::ShrImm { src, imm, .. } => (*src, LinearCopyOp::ShiftRight(*imm)),
        MInst::ShlImm { src, imm, .. } => (*src, LinearCopyOp::ShiftLeft(*imm)),
        MInst::LoadImm { value: 0, .. } => {
            return Some(LinearBitCopy {
                expression: value,
                source: value,
                shift: 0,
                output_mask: 0,
            });
        }
        // An OR below a field extraction is deliberately treated as one
        // opaque source. This lets a reconstructed value fold back to the
        // already-available packed value instead of duplicating its inputs.
        _ => {
            return Some(LinearBitCopy {
                expression: value,
                source: value,
                shift: 0,
                output_mask: u64::MAX,
            });
        }
    };

    let mut copy = trace_linear_bit_copy(source, instructions, definitions, depth + 1)?;
    match operation {
        LinearCopyOp::Mask(mask) => copy.output_mask &= mask,
        LinearCopyOp::ShiftRight(shift) => {
            copy.output_mask >>= shift;
            copy.shift -= i16::from(shift);
        }
        LinearCopyOp::ShiftLeft(shift) => {
            copy.output_mask <<= shift;
            copy.shift += i16::from(shift);
        }
    }
    Some(copy)
}

#[derive(Clone, Copy)]
enum LinearCopyOp {
    Mask(u64),
    ShiftRight(u8),
    ShiftLeft(u8),
}

pub(super) fn eliminate_redundant_or_terms(func: &mut MFunction) {
    for block in &mut func.blocks {
        let mut mov_aliases: HashMap<VReg, VReg> = HashMap::default();
        let mut rewrite_aliases: HashMap<VReg, VReg> = HashMap::default();
        let mut select_terms: HashMap<VReg, SelectTerm> = HashMap::default();
        let mut or_terms: HashMap<VReg, HashSet<SelectTerm>> = HashMap::default();

        for inst in &mut block.insts {
            if !rewrite_aliases.is_empty() {
                rewrite_uses(inst, &rewrite_aliases);
            }

            match inst {
                MInst::Mov { dst, src } => {
                    let canonical = resolve_alias(*src, &mov_aliases);
                    mov_aliases.insert(*dst, canonical);
                    if let Some(term) = select_terms.get(&canonical).copied() {
                        select_terms.insert(*dst, term);
                    }
                    if let Some(terms) = or_terms.get(&canonical).cloned() {
                        or_terms.insert(*dst, terms);
                    }
                }
                MInst::Select {
                    dst,
                    cond,
                    true_val,
                    false_val,
                } => {
                    let term = SelectTerm {
                        cond: resolve_alias(*cond, &mov_aliases),
                        true_val: resolve_alias(*true_val, &mov_aliases),
                        false_val: resolve_alias(*false_val, &mov_aliases),
                    };
                    select_terms.insert(*dst, term);
                    mov_aliases.remove(dst);
                    or_terms.remove(dst);
                }
                MInst::Or { dst, lhs, rhs } => {
                    let lhs = resolve_alias(*lhs, &rewrite_aliases);
                    let rhs = resolve_alias(*rhs, &rewrite_aliases);
                    let lhs_terms = or_terms.get(&lhs).cloned();
                    let rhs_terms = or_terms.get(&rhs).cloned();
                    let lhs_term = select_terms.get(&lhs).copied();
                    let rhs_term = select_terms.get(&rhs).copied();

                    let replacement = lhs_terms
                        .as_ref()
                        .and_then(|terms| rhs_term.filter(|term| terms.contains(term)).map(|_| lhs))
                        .or_else(|| {
                            rhs_terms.as_ref().and_then(|terms| {
                                lhs_term.filter(|term| terms.contains(term)).map(|_| rhs)
                            })
                        });

                    if let Some(src) = replacement {
                        let dst_vreg = *dst;
                        *inst = MInst::Mov { dst: dst_vreg, src };
                        rewrite_aliases.insert(dst_vreg, src);
                        mov_aliases.insert(dst_vreg, src);
                        if let Some(terms) = or_terms.get(&src).cloned() {
                            or_terms.insert(dst_vreg, terms);
                        }
                        continue;
                    }

                    let mut terms = lhs_terms.unwrap_or_default();
                    if let Some(rhs_terms) = rhs_terms {
                        terms.extend(rhs_terms);
                    }
                    if let Some(term) = lhs_term {
                        terms.insert(term);
                    }
                    if let Some(term) = rhs_term {
                        terms.insert(term);
                    }
                    if terms.is_empty() {
                        or_terms.remove(dst);
                    } else {
                        or_terms.insert(*dst, terms);
                    }
                    mov_aliases.remove(dst);
                    select_terms.remove(dst);
                }
                _ => {
                    if let Some(dst) = inst.def() {
                        mov_aliases.remove(&dst);
                        select_terms.remove(&dst);
                        or_terms.remove(&dst);
                    }
                }
            }
        }
    }
}
