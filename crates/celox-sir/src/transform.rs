use std::collections::BTreeSet;

use crate::{
    BasicBlock, BlockId, ExecutionUnit, HashMap, HashSet, RegisterId, SIRInstruction, SIROffset,
    SIRSwitchCase, SIRTerminator,
};

/// Merge multiple SIR ExecutionUnits into a single EU.
/// Each EU's Return becomes a Jump to the next EU's entry block.
/// RegisterIds and BlockIds are renumbered to avoid conflicts.
/// Returns (merged_eu, eu_entry_block_ids) where `eu_entry_block_ids[i]` is the
/// BlockId of the i-th EU's entry block in the merged EU (for i > 0).
pub fn merge_sir_eus<A: Clone>(units: &[ExecutionUnit<A>]) -> (ExecutionUnit<A>, Vec<BlockId>) {
    let units = units.iter().collect::<Vec<_>>();
    merge_sir_eu_refs(&units)
}

/// Exact source-unit provenance for a merged SIR function.
///
/// Unlike a list of boundary block IDs, this remains valid when an input
/// execution unit has a nonzero entry ID or sparse block IDs.
#[derive(Debug, Clone)]
pub struct SirMergeProvenance {
    pub unit_entries: Vec<BlockId>,
    pub block_units: HashMap<BlockId, usize>,
}

/// Reference-based variant of [`merge_sir_eus`] used when one compilation
/// unit is assembled from multiple Program-owned EU slices.
pub fn merge_sir_eu_refs<A: Clone>(
    units: &[&ExecutionUnit<A>],
) -> (ExecutionUnit<A>, Vec<BlockId>) {
    let (merged, provenance) = merge_sir_eu_refs_with_provenance(units);
    (merged, provenance.unit_entries[1..].to_vec())
}
pub fn merge_sir_eu_refs_with_provenance<A: Clone>(
    units: &[&ExecutionUnit<A>],
) -> (ExecutionUnit<A>, SirMergeProvenance) {
    assert!(!units.is_empty(), "cannot merge an empty SIR EU list");
    if units.len() == 1 {
        return (
            (*units[0]).clone(),
            SirMergeProvenance {
                unit_entries: vec![units[0].entry_block_id],
                block_units: units[0]
                    .blocks
                    .keys()
                    .copied()
                    .map(|block| (block, 0))
                    .collect(),
            },
        );
    }

    let mut merged_blocks = HashMap::default();
    let mut merged_regs = HashMap::default();
    let mut block_units = HashMap::default();

    // Compute offsets for renumbering
    let mut reg_offset = 0usize;
    let mut block_offset = 0usize;
    let mut reg_offsets = Vec::new();
    let mut block_offsets = Vec::new();

    for eu in units {
        reg_offsets.push(reg_offset);
        block_offsets.push(block_offset);
        let max_reg = eu.register_map.keys().map(|r| r.0).max().unwrap_or(0);
        reg_offset += max_reg + 1;
        let max_block = eu.blocks.keys().map(|b| b.0).max().unwrap_or(0);
        block_offset += max_block + 1;
    }

    // Entry block IDs after renumbering
    let entry_blocks: Vec<BlockId> = units
        .iter()
        .enumerate()
        .map(|(i, eu)| BlockId(eu.entry_block_id.0 + block_offsets[i]))
        .collect();

    for (eu_idx, eu) in units.iter().enumerate() {
        let ro = reg_offsets[eu_idx];
        let bo = block_offsets[eu_idx];
        let is_last = eu_idx == units.len() - 1;
        let next_entry = if !is_last {
            Some(entry_blocks[eu_idx + 1])
        } else {
            None
        };

        // Copy registers with offset
        for (&reg_id, reg_type) in &eu.register_map {
            merged_regs.insert(RegisterId(reg_id.0 + ro), reg_type.clone());
        }

        // Copy blocks with renumbering
        for (&block_id, block) in &eu.blocks {
            let new_block_id = BlockId(block_id.0 + bo);
            block_units.insert(new_block_id, eu_idx);
            let r = |reg: RegisterId| RegisterId(reg.0 + ro);
            let b = |bid: BlockId| BlockId(bid.0 + bo);

            let new_params: Vec<RegisterId> = block.params.iter().map(|p| r(*p)).collect();
            let new_insts: Vec<SIRInstruction<A>> = block
                .instructions
                .iter()
                .map(|inst| renumber_sir_inst(inst, ro, bo))
                .collect();

            let new_terminator = match &block.terminator {
                SIRTerminator::Return => {
                    if is_last {
                        SIRTerminator::Return
                    } else {
                        SIRTerminator::Jump(next_entry.unwrap(), vec![])
                    }
                }
                SIRTerminator::Error(code) => SIRTerminator::Error(*code),
                SIRTerminator::Jump(target, args) => {
                    let new_args: Vec<RegisterId> = args.iter().map(|a| r(*a)).collect();
                    SIRTerminator::Jump(b(*target), new_args)
                }
                SIRTerminator::Branch {
                    cond,
                    true_block,
                    false_block,
                } => SIRTerminator::Branch {
                    cond: r(*cond),
                    true_block: (
                        b(true_block.0),
                        true_block.1.iter().map(|a| r(*a)).collect(),
                    ),
                    false_block: (
                        b(false_block.0),
                        false_block.1.iter().map(|a| r(*a)).collect(),
                    ),
                },
                SIRTerminator::Switch {
                    selector,
                    cases,
                    default,
                } => SIRTerminator::Switch {
                    selector: r(*selector),
                    cases: cases
                        .iter()
                        .map(|case| SIRSwitchCase {
                            value: case.value.clone(),
                            target: b(case.target),
                        })
                        .collect(),
                    default: b(*default),
                },
            };

            merged_blocks.insert(
                new_block_id,
                BasicBlock {
                    id: new_block_id,
                    params: new_params,
                    instructions: new_insts,
                    terminator: new_terminator,
                },
            );
        }
    }

    (
        ExecutionUnit {
            entry_block_id: entry_blocks[0],
            blocks: merged_blocks,
            register_map: merged_regs,
        },
        SirMergeProvenance {
            unit_entries: entry_blocks,
            block_units,
        },
    )
}
pub fn inline_single_predecessor_jumps<A: Clone>(
    eu: &mut ExecutionUnit<A>,
) -> Result<bool, crate::verify::SirVerifyError> {
    fn successors(terminator: &SIRTerminator) -> Vec<BlockId> {
        match terminator {
            SIRTerminator::Jump(target, _) => vec![*target],
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => vec![true_block.0, false_block.0],
            SIRTerminator::Switch { cases, default, .. } => cases
                .iter()
                .map(|case| case.target)
                .chain(std::iter::once(*default))
                .collect(),
            SIRTerminator::Return | SIRTerminator::Error(_) => Vec::new(),
        }
    }

    let mut changed = false;
    let mut parameter_replacements = HashMap::<RegisterId, RegisterId>::default();
    let mut predecessor_count = HashMap::<BlockId, usize>::default();
    for block in eu.blocks.values() {
        for successor in successors(&block.terminator) {
            *predecessor_count.entry(successor).or_default() += 1;
        }
    }

    // Inlining `source -> target` replaces every outgoing edge of `target`
    // with the corresponding edge from `source`. Therefore predecessor edge
    // counts of all surviving blocks stay unchanged. Only `source` gets a new
    // terminator and can become a new candidate. A deterministic worklist
    // avoids rebuilding and sorting the whole CFG after every inlining step.
    let mut candidates = eu.blocks.keys().copied().collect::<BTreeSet<_>>();
    while let Some(block_id) = candidates.pop_first() {
        let Some(block) = eu.blocks.get(&block_id) else {
            continue;
        };
        let SIRTerminator::Jump(target, args) = &block.terminator else {
            continue;
        };
        if *target == block_id
            || *target == eu.entry_block_id
            || predecessor_count.get(target).copied().unwrap_or(0) != 1
        {
            continue;
        }
        let Some(target_block) = eu.blocks.get(target) else {
            continue;
        };
        if target_block.params.len() != args.len() {
            continue;
        }
        let target_id = *target;
        let args = args.clone();

        let target = eu
            .blocks
            .remove(&target_id)
            .expect("inline target exists when selected");
        candidates.remove(&target_id);
        for (parameter, argument) in target.params.iter().copied().zip(args) {
            parameter_replacements.insert(parameter, argument);
        }

        let block = eu
            .blocks
            .get_mut(&block_id)
            .expect("inline predecessor exists when selected");
        block.instructions.extend(target.instructions);
        block.terminator = target.terminator;
        candidates.insert(block_id);
        changed = true;
    }

    // A block parameter is an SSA definition whose uses may appear in any
    // block dominated by the removed block. Resolve chains of removed
    // parameters once, then rewrite the surviving unit in one linear pass.
    let mut flattened = HashMap::<RegisterId, RegisterId>::default();
    let mut parameters = parameter_replacements.keys().copied().collect::<Vec<_>>();
    parameters.sort_unstable();
    for parameter in parameters {
        if flattened.contains_key(&parameter) {
            continue;
        }
        let mut path = Vec::new();
        let mut on_path = HashSet::default();
        let mut current = parameter;
        let resolved = loop {
            if let Some(&resolved) = flattened.get(&current) {
                break resolved;
            }
            let Some(&next) = parameter_replacements.get(&current) else {
                break current;
            };
            if !on_path.insert(current) {
                return Err(crate::verify::SirVerifyError {
                    invariant: "SSA.INLINE_PARAMETER_ACYCLIC",
                    block: None,
                    instruction: None,
                    message: format!("single-predecessor parameter cycle reaches r{}", current.0),
                });
            }
            path.push(current);
            current = next;
        };
        for register in path {
            flattened.insert(register, resolved);
        }
    }
    if !flattened.is_empty() {
        for block in eu.blocks.values_mut() {
            for instruction in &mut block.instructions {
                replace_sir_uses(instruction, &flattened);
            }
            replace_sir_terminator_uses(&mut block.terminator, &flattened);
        }
    }
    if let Err(mut error) = eu.verify_result() {
        error.message = format!("after single-predecessor inlining: {}", error.message);
        return Err(error);
    }
    Ok(changed)
}
fn replace_sir_offset_uses(offset: &mut SIROffset, replacements: &HashMap<RegisterId, RegisterId>) {
    match offset {
        SIROffset::Static(_) | SIROffset::PackedElements { .. } => {}
        SIROffset::Dynamic(register) => {
            if let Some(&replacement) = replacements.get(register) {
                *register = replacement;
            }
        }
        SIROffset::Element {
            index,
            dynamic_bit_offset,
            ..
        } => {
            if let Some(&replacement) = replacements.get(index) {
                *index = replacement;
            }
            if let Some(register) = dynamic_bit_offset {
                if let Some(&replacement) = replacements.get(register) {
                    *register = replacement;
                }
            }
        }
    }
}
fn replace_sir_uses<A>(
    instruction: &mut SIRInstruction<A>,
    replacements: &HashMap<RegisterId, RegisterId>,
) {
    let replace = |register: &mut RegisterId| {
        if let Some(&replacement) = replacements.get(register) {
            *register = replacement;
        }
    };
    match instruction {
        SIRInstruction::Imm(..) => {}
        SIRInstruction::Binary(_, lhs, _, rhs) => {
            replace(lhs);
            replace(rhs);
        }
        SIRInstruction::Unary(_, _, source) | SIRInstruction::Slice(_, source, _, _) => {
            replace(source);
        }
        SIRInstruction::Load(_, _, offset, _) => {
            replace_sir_offset_uses(offset, replacements);
        }
        SIRInstruction::Store(_, offset, _, source, _, _) => {
            replace_sir_offset_uses(offset, replacements);
            replace(source);
        }
        SIRInstruction::Commit(_, _, offset, _, _) => {
            replace_sir_offset_uses(offset, replacements);
        }
        SIRInstruction::Concat(_, sources) => {
            for source in sources {
                replace(source);
            }
        }
        SIRInstruction::Mux(_, condition, then_value, else_value) => {
            replace(condition);
            replace(then_value);
            replace(else_value);
        }
        SIRInstruction::RuntimeEvent { args, .. } => {
            for arg in args {
                replace(arg);
            }
        }
        SIRInstruction::CombCaptureEvent { args, .. } => {
            for arg in args {
                replace(arg);
            }
        }
        SIRInstruction::CombCaptureEnableIfChanged { old, new, .. } => {
            replace(old);
            replace(new);
        }
    }
}
fn replace_sir_terminator_uses(
    terminator: &mut SIRTerminator,
    replacements: &HashMap<RegisterId, RegisterId>,
) {
    let replace = |register: &mut RegisterId| {
        if let Some(&replacement) = replacements.get(register) {
            *register = replacement;
        }
    };
    match terminator {
        SIRTerminator::Jump(_, args) => {
            for arg in args {
                replace(arg);
            }
        }
        SIRTerminator::Branch {
            cond,
            true_block,
            false_block,
        } => {
            replace(cond);
            for arg in &mut true_block.1 {
                replace(arg);
            }
            for arg in &mut false_block.1 {
                replace(arg);
            }
        }
        SIRTerminator::Switch {
            selector,
            cases: _,
            default: _,
        } => {
            replace(selector);
        }
        SIRTerminator::Return | SIRTerminator::Error(_) => {}
    }
}
fn renumber_sir_inst<A: Clone>(
    inst: &SIRInstruction<A>,
    ro: usize,
    _bo: usize,
) -> SIRInstruction<A> {
    let r = |reg: RegisterId| RegisterId(reg.0 + ro);
    let off = |o: &SIROffset| match o {
        SIROffset::Static(v) => SIROffset::Static(*v),
        SIROffset::Dynamic(reg) => SIROffset::Dynamic(r(*reg)),
        SIROffset::Element {
            index,
            element_width,
            bit_offset,
            dynamic_bit_offset,
        } => SIROffset::Element {
            index: r(*index),
            element_width: *element_width,
            bit_offset: *bit_offset,
            dynamic_bit_offset: dynamic_bit_offset.map(r),
        },
        SIROffset::PackedElements {
            bit_offset,
            element_width,
        } => SIROffset::PackedElements {
            bit_offset: *bit_offset,
            element_width: *element_width,
        },
    };

    match inst {
        SIRInstruction::Imm(dst, val) => SIRInstruction::Imm(r(*dst), val.clone()),
        SIRInstruction::Load(dst, addr, offset, width) => {
            SIRInstruction::Load(r(*dst), addr.clone(), off(offset), *width)
        }
        SIRInstruction::Store(addr, offset, width, src, triggers, comb_capture_sites) => {
            SIRInstruction::Store(
                addr.clone(),
                off(offset),
                *width,
                r(*src),
                triggers.clone(),
                comb_capture_sites.clone(),
            )
        }
        SIRInstruction::Commit(src, dst, offset, width, triggers) => SIRInstruction::Commit(
            src.clone(),
            dst.clone(),
            off(offset),
            *width,
            triggers.clone(),
        ),
        SIRInstruction::Binary(dst, lhs, op, rhs) => {
            SIRInstruction::Binary(r(*dst), r(*lhs), *op, r(*rhs))
        }
        SIRInstruction::Unary(dst, op, src) => SIRInstruction::Unary(r(*dst), *op, r(*src)),
        SIRInstruction::Concat(dst, args) => {
            SIRInstruction::Concat(r(*dst), args.iter().map(|a| r(*a)).collect())
        }
        SIRInstruction::Slice(dst, src, offset, width) => {
            SIRInstruction::Slice(r(*dst), r(*src), *offset, *width)
        }
        SIRInstruction::Mux(dst, cond, then_val, else_val) => {
            SIRInstruction::Mux(r(*dst), r(*cond), r(*then_val), r(*else_val))
        }
        SIRInstruction::RuntimeEvent { site_id, args } => SIRInstruction::RuntimeEvent {
            site_id: *site_id,
            args: args.iter().map(|a| r(*a)).collect(),
        },
        SIRInstruction::CombCaptureEvent {
            site_id,
            args,
            fatal_error_code,
            consume_enabled,
        } => SIRInstruction::CombCaptureEvent {
            site_id: *site_id,
            args: args.iter().map(|a| r(*a)).collect(),
            fatal_error_code: *fatal_error_code,
            consume_enabled: *consume_enabled,
        },
        SIRInstruction::CombCaptureEnableIfChanged { old, new, sites } => {
            SIRInstruction::CombCaptureEnableIfChanged {
                old: r(*old),
                new: r(*new),
                sites: sites.clone(),
            }
        }
    }
}
