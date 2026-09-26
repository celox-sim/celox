//! Recover closed straight-line regions without crossing CFG or effect boundaries.

use super::{
    CodegenOptions, Kernel, MemoryObject, Result, UnrolledOptions, recover_independent_stores,
};
use crate::{
    BasicBlock, BinaryOp, BlockId, ExecutionUnit, HashMap, HashSet, RegisterId, SIRInstruction,
    SIRTerminator,
    analysis::{UseSite, collect_use_sites, visit_instruction_uses},
    transform::{renumber_sir_inst, renumber_sir_terminator},
};
use celox_analysis::polyhedral::{Error, ScheduleOptions, schedule};
use std::{hash::Hash, ops::Range};

#[derive(Clone, Debug)]
pub struct RegionOptions {
    /// Per-region recovery budget. Scheduling and scanning have separate budgets.
    pub unrolled: UnrolledOptions,
    /// Includes rejected candidates, bounding the number of recovery attempts.
    pub max_candidates: usize,
    pub max_instructions: usize,
    pub max_blocks: usize,
}

impl Default for RegionOptions {
    fn default() -> Self {
        Self {
            unrolled: UnrolledOptions::default(),
            max_candidates: 64,
            max_instructions: 100_000,
            max_blocks: 1024,
        }
    }
}

#[derive(Clone, Debug)]
pub struct RegionRejection {
    pub block: BlockId,
    pub instructions: Range<usize>,
    pub reason: Error,
}

#[derive(Clone, Debug)]
struct RecoveredRegion<A> {
    block: BlockId,
    instructions: Range<usize>,
    stores: usize,
    kernel: Kernel<A>,
}

/// A proof tied to an immutable copy of the source unit. Callers cannot splice
/// its kernels into a different unit or change the proven instruction ranges.
#[derive(Clone, Debug)]
pub struct RecoveredRegions<A> {
    source: ExecutionUnit<A>,
    regions: Vec<RecoveredRegion<A>>,
    pub rejections: Vec<RegionRejection>,
}

impl<A: Clone + Eq + Hash> RecoveredRegions<A> {
    pub fn len(&self) -> usize {
        self.regions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub fn store_count(&self) -> usize {
        self.regions.iter().map(|r| r.stores).sum()
    }

    /// Stable indices for choosing a subset of this source unit's regions.
    pub fn kernels(&self) -> impl ExactSizeIterator<Item = &Kernel<A>> {
        self.regions.iter().map(|r| &r.kernel)
    }

    /// Generate a candidate while retaining every original branch and effect.
    /// Failure leaves the source intact. `max_instructions` bounds the total
    /// generated instructions across all regions, excluding untouched source.
    /// Analysis work is bounded separately for each region and each stage.
    /// Legality alone does not imply profitability; benchmark the complete unit.
    pub fn lower_with_options(
        &self,
        scheduling: &ScheduleOptions,
        codegen: &CodegenOptions,
    ) -> Result<ExecutionUnit<A>> {
        self.lower_selected_with_options(&(0..self.len()).collect::<Vec<_>>(), scheduling, codegen)
    }

    /// Lower only the selected kernel indices, preserving the other regions
    /// verbatim. Selection is a caller policy, separate from the legality proof.
    pub fn lower_selected_with_options(
        &self,
        selected: &[usize],
        scheduling: &ScheduleOptions,
        codegen: &CodegenOptions,
    ) -> Result<ExecutionUnit<A>> {
        let mut selected = selected.to_vec();
        selected.sort_unstable();
        if selected.iter().any(|&i| i >= self.len()) || selected.windows(2).any(|p| p[0] == p[1]) {
            return Err(Error::Invalid("invalid or duplicate region selection"));
        }
        let mut unit = self.source.clone();
        let mut generated = 0usize;
        // Later ranges in the same original block must be split first.
        for index in selected.into_iter().rev() {
            let region = &self.regions[index];
            let chosen = schedule(region.kernel.region(), scheduling)?;
            let replacement =
                region
                    .kernel
                    .lower_with_options(&chosen, None, codegen, scheduling.max_work)?;
            generated = replacement
                .blocks
                .values()
                .try_fold(generated, |n, block| {
                    n.checked_add(block.instructions.len())
                        .ok_or(Error::WorkLimit)
                })?;
            if generated > codegen.max_instructions {
                return Err(Error::WorkLimit);
            }
            splice(&mut unit, region, replacement)?;
        }
        unit.verify_result()
            .map_err(|_| Error::Invalid("affine region splice failed verification"))?;
        Ok(unit)
    }
}

/// Find maximal instruction spans whose accesses are static and whose operations
/// the unrolled bridge understands. Effects, potential traps, indirect accesses
/// and partial writes are boundaries; they remain in the source CFG.
///
/// Every span must have at least two stores, no SSA live-outs, and no live-ins
/// other than immediate constants (which are rematerialized). In particular,
/// loads from before a Commit are never rematerialized after it. The existing
/// bridge then proves disjoint outputs and immutable inputs *within the span*.
/// Writes elsewhere in the unit are permitted because execution never crosses
/// a boundary. Distinct [`MemoryObject`] identities must remain non-aliasing.
///
/// This is opt-in, and does not recover scalar recurrences, general live-ins,
/// or regions spanning branches. Rejected spans retain their original code.
pub fn recover_independent_regions<A: Clone + Eq + Hash>(
    unit: &ExecutionUnit<A>,
    objects: &HashMap<A, MemoryObject>,
    options: &RegionOptions,
) -> Result<RecoveredRegions<A>> {
    let instructions = unit.blocks.values().try_fold(0usize, |n, b| {
        n.checked_add(b.instructions.len()).ok_or(Error::WorkLimit)
    })?;
    if instructions > options.max_instructions
        || unit.register_map.len() > options.max_instructions
        || unit.blocks.len() > options.max_blocks
    {
        return Err(Error::WorkLimit);
    }
    unit.verify_result()
        .map_err(|_| Error::Invalid("region source failed SIR verification"))?;
    let uses = collect_use_sites(unit);
    let definitions = unit
        .blocks
        .values()
        .flat_map(|b| &b.instructions)
        .filter_map(|i| i.defined_register().map(|r| (r, i)))
        .collect::<HashMap<_, _>>();
    let mut blocks = unit.blocks.keys().copied().collect::<Vec<_>>();
    blocks.sort_unstable();
    let mut regions = Vec::new();
    let mut rejections = Vec::new();
    let mut candidates = 0usize;
    for id in blocks {
        let block = &unit.blocks[&id];
        let mut start = 0;
        for end in 0..=block.instructions.len() {
            if end < block.instructions.len() && supported(&block.instructions[end], objects) {
                continue;
            }
            let range = start..end;
            start = end + 1;
            let stores = block.instructions[range.clone()]
                .iter()
                .filter(|i| matches!(i, SIRInstruction::Store(..)))
                .count();
            if stores < 2 {
                continue;
            }
            candidates += 1;
            if candidates > options.max_candidates {
                return Err(Error::WorkLimit);
            }
            let result = closed_span(unit, id, range.clone(), &uses, &definitions)
                .and_then(|span| recover_independent_stores(&span, objects, &options.unrolled));
            match result {
                Ok(kernel) => regions.push(RecoveredRegion {
                    block: id,
                    instructions: range,
                    stores,
                    kernel,
                }),
                Err(reason) => rejections.push(RegionRejection {
                    block: id,
                    instructions: range,
                    reason,
                }),
            }
        }
    }
    Ok(RecoveredRegions {
        source: unit.clone(),
        regions,
        rejections,
    })
}

fn supported<A: Eq + Hash>(
    instruction: &SIRInstruction<A>,
    objects: &HashMap<A, MemoryObject>,
) -> bool {
    match instruction {
        SIRInstruction::Store(a, offset, width, _, triggers, captures) => {
            triggers.is_empty()
                && captures.is_empty()
                && super::unrolled::cell(a, offset, *width, objects).is_ok()
        }
        SIRInstruction::Load(_, a, offset, width) => {
            super::unrolled::read_cell(a, offset, *width, objects).is_ok()
        }
        SIRInstruction::Imm(..)
        | SIRInstruction::Unary(..)
        | SIRInstruction::Concat(..)
        | SIRInstruction::Slice(..)
        | SIRInstruction::Mux(..) => true,
        SIRInstruction::Binary(_, _, op, _) => !matches!(
            op,
            BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS
        ),
        _ => false,
    }
}

fn closed_span<A: Clone>(
    unit: &ExecutionUnit<A>,
    block: BlockId,
    range: Range<usize>,
    uses: &HashMap<RegisterId, Vec<UseSite>>,
    definitions: &HashMap<RegisterId, &SIRInstruction<A>>,
) -> Result<ExecutionUnit<A>> {
    let source = &unit.blocks[&block].instructions[range.clone()];
    let defined = source
        .iter()
        .filter_map(SIRInstruction::defined_register)
        .collect::<HashSet<_>>();
    for register in &defined {
        if uses.get(register).is_some_and(|uses| {
            uses.iter().any(|site| {
                site.block != block || site.inst_idx.is_none_or(|index| !range.contains(&index))
            })
        }) {
            return Err(Error::Invalid("region has SSA live-outs"));
        }
    }
    let mut imported = HashSet::default();
    for instruction in source {
        visit_instruction_uses(instruction, |r| {
            if !defined.contains(&r) {
                imported.insert(r);
            }
        });
    }
    let mut imported = imported.into_iter().collect::<Vec<_>>();
    imported.sort_unstable();
    let mut instructions = Vec::new();
    for register in &imported {
        match definitions.get(register) {
            Some(instruction @ SIRInstruction::Imm(..)) => {
                instructions.push((*instruction).clone())
            }
            _ => return Err(Error::Invalid("region has nonconstant SSA live-ins")),
        }
    }
    instructions.extend_from_slice(source);
    let register_map = defined
        .into_iter()
        .chain(imported)
        .map(|r| (r, unit.register_map[&r].clone()))
        .collect();
    let block = BasicBlock {
        id: BlockId(0),
        params: vec![],
        instructions,
        terminator: SIRTerminator::Return,
    };
    Ok(ExecutionUnit {
        entry_block_id: block.id,
        blocks: [(block.id, block)].into_iter().collect(),
        register_map,
    })
}

fn next_id(ids: impl Iterator<Item = usize>) -> Result<usize> {
    ids.max().map_or(Ok(0), |id| {
        id.checked_add(1).ok_or(Error::ArithmeticOverflow)
    })
}

fn splice<A: Clone>(
    unit: &mut ExecutionUnit<A>,
    region: &RecoveredRegion<A>,
    replacement: ExecutionUnit<A>,
) -> Result<()> {
    let ro = next_id(unit.register_map.keys().map(|r| r.0))?;
    let bo = next_id(unit.blocks.keys().map(|b| b.0))?;
    ro.checked_add(next_id(replacement.register_map.keys().map(|r| r.0))?)
        .ok_or(Error::ArithmeticOverflow)?;
    let continuation = BlockId(
        bo.checked_add(next_id(replacement.blocks.keys().map(|b| b.0))?)
            .ok_or(Error::ArithmeticOverflow)?,
    );
    let entry = BlockId(replacement.entry_block_id.0 + bo);
    let source = unit.blocks.get_mut(&region.block).unwrap();
    let suffix = source.instructions.split_off(region.instructions.end);
    for instruction in source.instructions.drain(region.instructions.start..) {
        if let Some(register) = instruction.defined_register() {
            unit.register_map.remove(&register);
        }
    }
    let terminator = std::mem::replace(&mut source.terminator, SIRTerminator::Jump(entry, vec![]));
    unit.blocks.insert(
        continuation,
        BasicBlock {
            id: continuation,
            params: vec![],
            instructions: suffix,
            terminator,
        },
    );
    for (r, ty) in replacement.register_map {
        unit.register_map.insert(RegisterId(r.0 + ro), ty);
    }
    for (id, block) in replacement.blocks {
        let id = BlockId(id.0 + bo);
        let terminator = if block.terminator == SIRTerminator::Return {
            SIRTerminator::Jump(continuation, vec![])
        } else {
            renumber_sir_terminator(&block.terminator, ro, bo)
        };
        unit.blocks.insert(
            id,
            BasicBlock {
                id,
                params: block.params.iter().map(|r| RegisterId(r.0 + ro)).collect(),
                instructions: block
                    .instructions
                    .iter()
                    .map(|i| renumber_sir_inst(i, ro, bo))
                    .collect(),
                terminator,
            },
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RegisterType, SIRBuilder, SIROffset, SIRValue};

    fn fixture(kind: usize) -> (ExecutionUnit<u32>, HashMap<u32, MemoryObject>) {
        let mut b = SIRBuilder::new();
        let value = b.alloc_logic(32);
        if kind == 1 {
            b.emit(SIRInstruction::Load(value, 0, SIROffset::Static(0), 32));
        } else {
            b.emit(SIRInstruction::Imm(value, SIRValue::new(5u32)));
        }
        if kind <= 1 {
            b.emit(SIRInstruction::Commit(
                1,
                0,
                SIROffset::Static(0),
                64,
                vec![],
            ));
        }
        let value = if kind == 4 {
            let param = b.alloc_logic(32);
            let body = b.new_block_with(vec![param]);
            b.seal_block(SIRTerminator::Jump(body, vec![value]));
            b.switch_to_block(body);
            param
        } else {
            value
        };
        for i in 0..2 {
            b.emit(SIRInstruction::Store(
                1,
                SIROffset::Static(i * 32),
                32,
                value,
                vec![],
                vec![],
            ));
        }
        if kind == 2 {
            b.emit(SIRInstruction::RuntimeEvent {
                site_id: 0,
                args: vec![value],
            });
        }
        if kind == 3 {
            let param = b.alloc_logic(32);
            let exit = b.new_block_with(vec![param]);
            b.seal_block(SIRTerminator::Jump(exit, vec![value]));
            b.switch_to_block(exit);
        }
        b.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = b.drain();
        let unit = ExecutionUnit {
            entry_block_id: BlockId(0),
            blocks,
            register_map,
        };
        unit.verify();
        (
            unit,
            (0..2)
                .map(|a| {
                    (
                        a,
                        MemoryObject {
                            element_width: 32,
                            elements: 2,
                        },
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn constants_can_cross_effects_but_snapshots_and_live_outs_cannot() {
        for (kind, reason) in [
            (0, None),
            (1, Some("region has nonconstant SSA live-ins")),
            (2, Some("region has SSA live-outs")),
            (3, Some("region has SSA live-outs")),
            (4, Some("region has nonconstant SSA live-ins")),
        ] {
            let (unit, objects) = fixture(kind);
            let found = recover_independent_regions(&unit, &objects, &Default::default()).unwrap();
            if let Some(reason) = reason {
                assert!(found.is_empty());
                assert_eq!(found.rejections[0].reason, Error::Invalid(reason));
                assert_eq!(
                    found
                        .lower_with_options(&Default::default(), &Default::default())
                        .unwrap(),
                    unit
                );
            } else {
                assert_eq!(found.len(), 1);
                found
                    .lower_with_options(&Default::default(), &Default::default())
                    .unwrap()
                    .verify();
            }
        }
    }

    #[test]
    fn candidate_and_codegen_limits_leave_original_intact() {
        let (unit, objects) = fixture(0);
        for options in [
            RegionOptions {
                max_candidates: 0,
                ..Default::default()
            },
            RegionOptions {
                max_instructions: 1,
                ..Default::default()
            },
            RegionOptions {
                max_blocks: 0,
                ..Default::default()
            },
        ] {
            assert!(matches!(
                recover_independent_regions(&unit, &objects, &options),
                Err(Error::WorkLimit)
            ));
        }
        let mut sparse = unit.clone();
        let mut block = sparse.blocks.remove(&BlockId(0)).unwrap();
        block.id = BlockId(43);
        sparse.entry_block_id = block.id;
        sparse.blocks.insert(block.id, block);
        let found = recover_independent_regions(&sparse, &objects, &Default::default()).unwrap();
        for invalid in [vec![1], vec![0, 0]] {
            assert!(matches!(
                found.lower_selected_with_options(
                    &invalid,
                    &Default::default(),
                    &Default::default()
                ),
                Err(Error::Invalid("invalid or duplicate region selection"))
            ));
        }
        let generated = found
            .lower_with_options(&Default::default(), &Default::default())
            .unwrap();
        assert_eq!(generated.entry_block_id, BlockId(43));
        assert!(matches!(
            found.lower_with_options(
                &Default::default(),
                &CodegenOptions {
                    max_instructions: 0,
                    ..Default::default()
                }
            ),
            Err(Error::WorkLimit)
        ));
        sparse
            .register_map
            .insert(RegisterId(usize::MAX), RegisterType::Logic { width: 32 });
        let found = recover_independent_regions(&sparse, &objects, &Default::default()).unwrap();
        assert!(matches!(
            found.lower_with_options(&Default::default(), &Default::default()),
            Err(Error::ArithmeticOverflow)
        ));
        unit.verify();
    }
}
