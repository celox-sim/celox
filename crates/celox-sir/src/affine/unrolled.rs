//! A bounded bridge from fully unrolled array expressions to affine regions.
//!
//! This does not reconstruct source loop provenance. It proves a more limited
//! fact: every output cell is written once, and every load reads an object
//! untouched by the unit. Such stores commute. Structurally identical scalar
//! expressions, translated by the output index, can then form a counted loop.
//! Every concrete store participates in the proof; two sample lanes are not
//! sufficient. Ordinary optimization pipelines do not call this experiment.

use super::{Kernel, MemoryObject, Result, StatementBody};
use crate::{
    BinaryOp, ExecutionUnit, HashMap, HashSet, RegisterId, RegisterType, SIRInstruction, SIROffset,
    SIRTerminator, SIRValue, analysis::visit_instruction_uses,
};
use celox_analysis::polyhedral::{Affine, Domain, Error};
use std::hash::Hash;

#[derive(Clone, Debug)]
pub struct UnrolledOptions {
    pub max_statements: usize,
    /// Bounds preprocessing and the total number of visited expression nodes.
    pub max_work: usize,
}

impl Default for UnrolledOptions {
    fn default() -> Self {
        Self {
            max_statements: 8,
            max_work: 2_000_000,
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Template<A> {
    instructions: Vec<SIRInstruction<A>>,
    types: Vec<RegisterType>,
}

impl<A> Template<A> {
    fn alloc(&mut self, ty: RegisterType) -> RegisterId {
        let id = RegisterId(self.types.len());
        self.types.push(ty);
        id
    }

    fn translated_index(&mut self, delta: i64) -> RegisterId {
        if delta == 0 {
            return RegisterId(0);
        }
        let ty = RegisterType::Bit {
            width: 64,
            signed: true,
        };
        let constant = self.alloc(ty.clone());
        self.instructions
            .push(SIRInstruction::Imm(constant, SIRValue::new(delta as u64)));
        let index = self.alloc(ty);
        self.instructions.push(SIRInstruction::Binary(
            index,
            RegisterId(0),
            BinaryOp::Add,
            constant,
        ));
        index
    }
}

fn cell<A: Eq + Hash>(
    address: &A,
    offset: &SIROffset,
    width: usize,
    objects: &HashMap<A, MemoryObject>,
) -> Result<i64> {
    let object = objects
        .get(address)
        .ok_or(Error::Invalid("missing unrolled memory shape"))?;
    let bit_offset = match offset {
        SIROffset::Static(offset) => *offset,
        SIROffset::PackedElements {
            bit_offset,
            element_width,
        } if *element_width == width => *bit_offset,
        _ => {
            return Err(Error::Invalid(
                "unrolled recovery requires static complete cells",
            ));
        }
    };
    if width == 0
        || width != object.element_width
        || !bit_offset.is_multiple_of(width)
        || bit_offset / width >= object.elements
    {
        return Err(Error::Invalid(
            "unrolled recovery requires in-bounds complete cells",
        ));
    }
    i64::try_from(bit_offset / width).map_err(|_| Error::ArithmeticOverflow)
}

/// Recover unit-stride expression families from a complete straight-line unit.
///
/// All accesses must address static complete cells. Output cells must be
/// disjoint, and input objects immutable throughout the unit. Captures, events,
/// triggers, commits, control flow and division/remainder are rejected. Memory
/// identities carry the same disjointness contract as [`MemoryObject`].
///
/// This bridge can lose existing cross-lane CSE and vectorization. Successful
/// recovery establishes legality, not profitability; callers must benchmark
/// and keep the original unit as a candidate.
pub fn recover_independent_stores<A: Clone + Eq + Hash>(
    unit: &ExecutionUnit<A>,
    objects: &HashMap<A, MemoryObject>,
    options: &UnrolledOptions,
) -> Result<Kernel<A>> {
    let Some(block) = unit.blocks.get(&unit.entry_block_id) else {
        return Err(Error::Invalid("unrolled entry block"));
    };
    if unit.blocks.len() != 1
        || !block.params.is_empty()
        || block.terminator != SIRTerminator::Return
    {
        return Err(Error::Invalid(
            "unrolled recovery requires a straight-line unit",
        ));
    }
    let mut work = options
        .max_work
        .checked_sub(block.instructions.len())
        .ok_or(Error::WorkLimit)?;
    unit.verify_result()
        .map_err(|_| Error::Invalid("unrolled SIR failed verification"))?;
    let mut written = HashSet::default();
    let mut cells = HashSet::default();
    let mut definitions = HashMap::default();
    let mut stores = Vec::new();
    for instruction in &block.instructions {
        if let Some(register) = instruction.defined_register() {
            definitions.insert(register, instruction);
        }
        match instruction {
            SIRInstruction::Store(address, offset, width, value, triggers, captures)
                if triggers.is_empty() && captures.is_empty() =>
            {
                let index = cell(address, offset, *width, objects)?;
                if !cells.insert((address.clone(), index)) {
                    return Err(Error::Invalid("unrolled stores overlap"));
                }
                written.insert(address.clone());
                stores.push((address, *width, *value, index));
            }
            SIRInstruction::Load(_, address, offset, width) => {
                cell(address, offset, *width, objects)?;
            }
            SIRInstruction::Imm(..)
            | SIRInstruction::Unary(..)
            | SIRInstruction::Concat(..)
            | SIRInstruction::Slice(..)
            | SIRInstruction::Mux(..) => {}
            SIRInstruction::Binary(_, _, op, _)
                if !matches!(
                    op,
                    BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS
                ) => {}
            _ => {
                return Err(Error::Invalid(
                    "unrolled recovery rejects effects and potential traps",
                ));
            }
        }
    }
    for instruction in &block.instructions {
        if let SIRInstruction::Load(_, address, ..) = instruction
            && written.contains(address)
        {
            return Err(Error::Invalid("unrolled input is also written"));
        }
    }
    let mut lookup = HashMap::<Template<A>, usize>::default();
    let mut groups = Vec::<(Template<A>, Vec<i64>)>::new();
    for (address, width, value, index) in stores {
        let mut template = Template {
            instructions: Vec::new(),
            types: vec![RegisterType::Bit {
                width: 64,
                signed: true,
            }],
        };
        let mut registers = HashMap::default();
        // Iterative postorder keeps deeply expanded expressions off the stack.
        let mut pending = vec![(value, false)];
        while let Some((register, expanded)) = pending.pop() {
            work = work.checked_sub(1).ok_or(Error::WorkLimit)?;
            if registers.contains_key(&register) {
                continue;
            }
            let source = definitions[&register];
            if !expanded {
                pending.push((register, true));
                let mut children = Vec::new();
                visit_instruction_uses(source, |child| children.push((child, false)));
                pending.extend(children.into_iter().rev());
                continue;
            }
            let r = |old: &RegisterId| registers[old];
            // Allocate operands of a translated address before its load result
            // so normalized register names do not depend on frontend IDs.
            let offset = if let SIRInstruction::Load(_, address, offset, width) = source {
                let original = cell(address, offset, *width, objects)?;
                Some(if objects[address].elements == 1 {
                    SIROffset::Static(0)
                } else {
                    SIROffset::Element {
                        index: template.translated_index(
                            original
                                .checked_sub(index)
                                .ok_or(Error::ArithmeticOverflow)?,
                        ),
                        element_width: *width,
                        bit_offset: 0,
                        dynamic_bit_offset: None,
                    }
                })
            } else {
                None
            };
            let dst = template.alloc(unit.register_map[&register].clone());
            let normalized = match source {
                SIRInstruction::Imm(_, value) => SIRInstruction::Imm(dst, value.clone()),
                SIRInstruction::Binary(_, a, op, b) => SIRInstruction::Binary(dst, r(a), *op, r(b)),
                SIRInstruction::Unary(_, op, a) => SIRInstruction::Unary(dst, *op, r(a)),
                SIRInstruction::Load(_, address, _, width) => {
                    SIRInstruction::Load(dst, address.clone(), offset.unwrap(), *width)
                }
                SIRInstruction::Concat(_, args) => {
                    SIRInstruction::Concat(dst, args.iter().map(r).collect())
                }
                SIRInstruction::Slice(_, a, start, width) => {
                    SIRInstruction::Slice(dst, r(a), *start, *width)
                }
                SIRInstruction::Mux(_, cond, yes, no) => {
                    SIRInstruction::Mux(dst, r(cond), r(yes), r(no))
                }
                _ => return Err(Error::Invalid("unrolled expression definition")),
            };
            template.instructions.push(normalized);
            registers.insert(register, dst);
        }
        template.instructions.push(SIRInstruction::Store(
            address.clone(),
            SIROffset::Element {
                index: RegisterId(0),
                element_width: width,
                bit_offset: 0,
                dynamic_bit_offset: None,
            },
            width,
            registers[&value],
            vec![],
            vec![],
        ));
        if let Some(&group) = lookup.get(&template) {
            groups[group].1.push(index);
        } else {
            if groups.len() >= options.max_statements {
                return Err(Error::WorkLimit);
            }
            lookup.insert(template.clone(), groups.len());
            groups.push((template, vec![index]));
        }
    }
    let mut bodies = Vec::new();
    let mut repeated = false;
    for (template, mut indices) in groups {
        indices.sort_unstable();
        let mut cursor = 0;
        while cursor < indices.len() {
            let begin = cursor;
            cursor += 1;
            while cursor < indices.len() && indices[cursor] == indices[cursor - 1] + 1 {
                cursor += 1;
            }
            if bodies.len() >= options.max_statements {
                return Err(Error::WorkLimit);
            }
            repeated |= cursor - begin > 1;
            let end = indices[cursor - 1]
                .checked_add(1)
                .ok_or(Error::ArithmeticOverflow)?;
            bodies.push(StatementBody {
                domain: Domain::rectangular(vec![indices[begin]..end]),
                // The immutable-input/disjoint-output proof permits this
                // grouping even if the original stores were interleaved.
                original_schedule: vec![
                    Affine::constant(1, bodies.len() as i64),
                    Affine::axis(1, 0),
                ],
                induction: vec![RegisterId(0)],
                register_types: template
                    .types
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(id, ty)| (RegisterId(id), ty))
                    .collect(),
                instructions: template.instructions.clone(),
            });
        }
    }
    if !repeated {
        return Err(Error::Invalid("no repeated unrolled array stores"));
    }
    Kernel::from_bodies(bodies, objects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockId, SIRBuilder};

    fn fixture(indices: &[usize]) -> (ExecutionUnit<u32>, HashMap<u32, MemoryObject>) {
        let mut builder = SIRBuilder::new();
        for &index in indices {
            let value = builder.alloc_logic(32);
            builder.emit(SIRInstruction::Load(
                value,
                0,
                SIROffset::Static(index * 32),
                32,
            ));
            builder.emit(SIRInstruction::Store(
                1,
                SIROffset::Static(index * 32),
                32,
                value,
                vec![],
                vec![],
            ));
        }
        builder.seal_block(SIRTerminator::Return);
        let (blocks, register_map, _) = builder.drain();
        let unit = ExecutionUnit {
            blocks,
            register_map,
            entry_block_id: BlockId(0),
        };
        let objects = [0, 1]
            .into_iter()
            .map(|address| {
                (
                    address,
                    MemoryObject {
                        element_width: 32,
                        elements: 16,
                    },
                )
            })
            .collect();
        (unit, objects)
    }

    #[test]
    fn preserves_holes_and_checks_every_lane() {
        let (mut unit, objects) = fixture(&[5, 4, 1, 0, 9]);
        // A late lane differs from the first four, even though it has the
        // same destination object and complete-cell width.
        let instructions = &mut unit.blocks.get_mut(&BlockId(0)).unwrap().instructions;
        if let SIRInstruction::Load(_, _, offset, _) = &mut instructions[8] {
            *offset = SIROffset::Static(8 * 32);
        }
        let kernel = recover_independent_stores(&unit, &objects, &Default::default()).unwrap();
        assert_eq!(
            kernel
                .region()
                .statements
                .iter()
                .map(|s| s.domain.bounds[0].clone())
                .collect::<Vec<_>>(),
            vec![0..2, 4..6, 9..10]
        );
        assert_eq!(
            kernel.region().statements[2].accesses[0].subscripts[0],
            Affine::new(vec![1], -1)
        );
        kernel
            .lower_original(2_000_000)
            .unwrap()
            .verify_result()
            .unwrap();
        assert_eq!(
            recover_independent_stores(
                &unit,
                &objects,
                &UnrolledOptions {
                    max_statements: 2,
                    ..Default::default()
                }
            )
            .unwrap_err(),
            Error::WorkLimit
        );
        assert_eq!(
            recover_independent_stores(
                &unit,
                &objects,
                &UnrolledOptions {
                    max_work: 0,
                    ..Default::default()
                }
            )
            .unwrap_err(),
            Error::WorkLimit
        );
    }

    #[test]
    fn rejects_overlapping_writes_mutable_inputs_and_effects() {
        let (unit, objects) = fixture(&[0, 1, 2]);
        for mutation in 0..5 {
            let mut changed = unit.clone();
            let instructions = &mut changed.blocks.get_mut(&BlockId(0)).unwrap().instructions;
            match mutation {
                0 => {
                    if let SIRInstruction::Store(_, offset, ..) = &mut instructions[3] {
                        *offset = SIROffset::Static(0);
                    }
                }
                1 => {
                    if let SIRInstruction::Store(address, ..) = &mut instructions[1] {
                        *address = 0;
                    }
                }
                2 => {
                    if let SIRInstruction::Store(_, _, width, ..) = &mut instructions[1] {
                        *width = 16;
                    }
                }
                3 => {
                    if let SIRInstruction::Store(_, _, _, _, _, captures) = &mut instructions[1] {
                        captures.push(0);
                    }
                }
                4 => instructions.push(SIRInstruction::RuntimeEvent {
                    site_id: 0,
                    args: vec![],
                }),
                _ => unreachable!(),
            }
            assert!(
                recover_independent_stores(&changed, &objects, &Default::default()).is_err(),
                "mutation={mutation}"
            );
        }
    }
}
