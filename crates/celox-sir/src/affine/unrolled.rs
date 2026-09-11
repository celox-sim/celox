//! A bounded bridge from fully unrolled array expressions to affine regions.
//!
//! This does not reconstruct source loop provenance. It proves a more limited
//! fact: every output cell is written once, and every load reads an object
//! untouched by the unit. Such stores commute. Structurally identical scalar
//! expressions with affine input subscripts can then form a counted loop.
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

    fn affine_index(&mut self, expression: &Affine) -> RegisterId {
        let ty = RegisterType::Bit {
            width: 64,
            signed: true,
        };
        let coefficient = expression.coefficients[0];
        if coefficient == 0 {
            let index = self.alloc(ty);
            self.instructions.push(SIRInstruction::Imm(
                index,
                SIRValue::new(expression.constant as u64),
            ));
            return index;
        }
        let mut base = RegisterId(0);
        if coefficient != 1 {
            let constant = self.alloc(ty.clone());
            self.instructions.push(SIRInstruction::Imm(
                constant,
                SIRValue::new(coefficient as u64),
            ));
            base = self.alloc(ty.clone());
            self.instructions.push(SIRInstruction::Binary(
                base,
                RegisterId(0),
                BinaryOp::Mul,
                constant,
            ));
        }
        if expression.constant == 0 {
            return base;
        }
        let constant = self.alloc(ty.clone());
        self.instructions.push(SIRInstruction::Imm(
            constant,
            SIRValue::new(expression.constant as u64),
        ));
        let index = self.alloc(ty);
        self.instructions
            .push(SIRInstruction::Binary(index, base, BinaryOp::Add, constant));
        index
    }

    fn with_load_indices(mut self, expressions: &[Affine]) -> Self {
        let instructions = std::mem::take(&mut self.instructions);
        let mut expressions = expressions.iter();
        for mut instruction in instructions {
            if let SIRInstruction::Load(_, _, offset, width) = &mut instruction {
                *offset = SIROffset::Element {
                    index: self.affine_index(expressions.next().unwrap()),
                    element_width: *width,
                    bit_offset: 0,
                    dynamic_bit_offset: None,
                };
            }
            self.instructions.push(instruction);
        }
        self
    }
}

struct Lane {
    output: i64,
    loads: Vec<i64>,
}

/// Two adjacent outputs propose a model; every lane in a region must then
/// match it. A mismatch starts another bounded piece, never extrapolates it.
fn load_models(first: &Lane, next: Option<&Lane>) -> Result<Vec<Affine>> {
    first
        .loads
        .iter()
        .enumerate()
        .map(|(load, &index)| {
            let slope = next.map_or(0, |next| next.loads[load] - index);
            let constant = i128::from(index) - i128::from(slope) * i128::from(first.output);
            Ok(Affine::new(
                vec![slope],
                i64::try_from(constant).map_err(|_| Error::ArithmeticOverflow)?,
            ))
        })
        .collect()
}

pub(super) fn cell<A: Eq + Hash>(
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

/// A pure read may cover part of one cell. Load that proven in-bounds cell
/// and retain the original slice; cross-cell reads remain unsupported.
pub(super) fn read_cell<A: Eq + Hash>(
    address: &A,
    offset: &SIROffset,
    width: usize,
    objects: &HashMap<A, MemoryObject>,
) -> Result<(i64, usize)> {
    let object = objects
        .get(address)
        .ok_or(Error::Invalid("missing unrolled memory shape"))?;
    let bit = match offset {
        SIROffset::Static(bit) => *bit,
        SIROffset::PackedElements {
            bit_offset,
            element_width,
        } if *element_width == object.element_width => *bit_offset,
        _ => return Err(Error::Invalid("unrolled recovery requires static reads")),
    };
    if object.element_width == 0
        || width == 0
        || bit / object.element_width >= object.elements
        || width > object.element_width - bit % object.element_width
    {
        return Err(Error::Invalid(
            "unrolled read crosses a cell or object boundary",
        ));
    }
    Ok((
        i64::try_from(bit / object.element_width).map_err(|_| Error::ArithmeticOverflow)?,
        bit % object.element_width,
    ))
}

/// Recover contiguous output families from a complete straight-line unit.
/// Input subscripts may have constant, positive or negative integer strides.
///
/// All accesses must be static; reads may select a slice of one cell. Output
/// cells must be disjoint, and input objects immutable throughout the unit.
/// Captures, events, triggers, commits, control flow and division/remainder are
/// rejected. Memory identities carry the same disjointness contract as [`MemoryObject`].
///
/// Recovery can lose existing cross-lane sharing and efficient native memory
/// accesses. It establishes legality, not profitability; callers must benchmark
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
                read_cell(address, offset, *width, objects)?;
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
    let mut groups = Vec::<(Template<A>, Vec<Lane>)>::new();
    for (address, width, value, index) in stores {
        let mut template = Template {
            instructions: Vec::new(),
            types: vec![RegisterType::Bit {
                width: 64,
                signed: true,
            }],
        };
        let mut loads = Vec::new();
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
            let dst = template.alloc(unit.register_map[&register].clone());
            let normalized = match source {
                SIRInstruction::Imm(_, value) => SIRInstruction::Imm(dst, value.clone()),
                SIRInstruction::Binary(_, a, op, b) => SIRInstruction::Binary(dst, r(a), *op, r(b)),
                SIRInstruction::Unary(_, op, a) => SIRInstruction::Unary(dst, *op, r(a)),
                SIRInstruction::Load(_, address, offset, width) => {
                    let (index, bit) = read_cell(address, offset, *width, objects)?;
                    loads.push(index);
                    // Address values are validated separately, not ignored:
                    // the template key captures the typed expression topology.
                    let full_width = objects[address].element_width;
                    if bit == 0 && *width == full_width {
                        SIRInstruction::Load(dst, address.clone(), SIROffset::Static(0), *width)
                    } else {
                        let ty = match unit.register_map[&register] {
                            RegisterType::Logic { .. } => RegisterType::Logic { width: full_width },
                            RegisterType::Bit { signed, .. } => RegisterType::Bit {
                                width: full_width,
                                signed,
                            },
                        };
                        let full = template.alloc(ty);
                        template.instructions.push(SIRInstruction::Load(
                            full,
                            address.clone(),
                            SIROffset::Static(0),
                            full_width,
                        ));
                        SIRInstruction::Slice(dst, full, bit, *width)
                    }
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
        let lane = Lane {
            output: index,
            loads,
        };
        if let Some(&group) = lookup.get(&template) {
            groups[group].1.push(lane);
        } else {
            if groups.len() >= options.max_statements {
                return Err(Error::WorkLimit);
            }
            lookup.insert(template.clone(), groups.len());
            groups.push((template, vec![lane]));
        }
    }
    let mut bodies = Vec::new();
    let mut repeated = false;
    for (template, mut lanes) in groups {
        lanes.sort_unstable_by_key(|lane| lane.output);
        let mut cursor = 0;
        while cursor < lanes.len() {
            let begin = cursor;
            let next = lanes
                .get(cursor + 1)
                .filter(|next| next.output == lanes[begin].output + 1);
            let models = load_models(&lanes[begin], next)?;
            cursor += 1;
            while cursor < lanes.len() && lanes[cursor].output == lanes[cursor - 1].output + 1 {
                work = work.checked_sub(models.len()).ok_or(Error::WorkLimit)?;
                let mut matches = true;
                for (model, &observed) in models.iter().zip(&lanes[cursor].loads) {
                    matches &= model.evaluate(&[lanes[cursor].output])? == observed;
                }
                if !matches {
                    break;
                }
                cursor += 1;
            }
            if bodies.len() >= options.max_statements {
                return Err(Error::WorkLimit);
            }
            repeated |= cursor - begin > 1;
            let end = lanes[cursor - 1]
                .output
                .checked_add(1)
                .ok_or(Error::ArithmeticOverflow)?;
            let body = template.clone().with_load_indices(&models);
            bodies.push(StatementBody {
                domain: Domain::rectangular(vec![lanes[begin].output..end]),
                // The immutable-input/disjoint-output proof permits this
                // grouping even if the original stores were interleaved.
                original_schedule: vec![
                    Affine::constant(1, bodies.len() as i64),
                    Affine::axis(1, 0),
                ],
                induction: vec![RegisterId(0)],
                register_types: body
                    .types
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(id, ty)| (RegisterId(id), ty))
                    .collect(),
                instructions: body.instructions,
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
            kernel.region().statements[2].accesses[0].subscripts[0]
                .evaluate(&[9])
                .unwrap(),
            8
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
    fn verifies_strided_reversed_and_broadcast_inputs_including_late_exceptions() {
        for (slope, constant) in [(2, 3), (-1, 30), (0, 7)] {
            let (mut unit, mut objects) = fixture(&(0..20).collect::<Vec<_>>());
            for object in objects.values_mut() {
                object.elements = 64;
            }
            for (index, pair) in unit
                .blocks
                .get_mut(&BlockId(0))
                .unwrap()
                .instructions
                .chunks_mut(2)
                .enumerate()
            {
                if let SIRInstruction::Load(_, _, offset, _) = &mut pair[0] {
                    *offset = SIROffset::Static((slope * index as i64 + constant) as usize * 32);
                }
            }
            let options = UnrolledOptions {
                max_statements: 1,
                ..Default::default()
            };
            let kernel = recover_independent_stores(&unit, &objects, &options).unwrap();
            assert_eq!(
                kernel.region().statements[0].accesses[0].subscripts[0],
                Affine::new(vec![slope], constant)
            );
            kernel
                .lower_original(2_000_000)
                .unwrap()
                .verify_result()
                .unwrap();
            if let SIRInstruction::Load(_, _, offset, _) =
                &mut unit.blocks.get_mut(&BlockId(0)).unwrap().instructions[38]
            {
                *offset = SIROffset::Static(60 * 32);
            }
            assert_eq!(
                recover_independent_stores(&unit, &objects, &options).unwrap_err(),
                Error::WorkLimit
            );
            let split = recover_independent_stores(&unit, &objects, &Default::default()).unwrap();
            assert_eq!(split.region().statements.len(), 2);
            assert_eq!(
                split.region().statements[1].accesses[0].subscripts[0]
                    .evaluate(&[19])
                    .unwrap(),
                60
            );
        }
    }

    #[test]
    fn rejects_overlapping_writes_mutable_inputs_and_effects() {
        let (unit, objects) = fixture(&[0, 1, 2]);
        for mutation in 0..6 {
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
                5 => {
                    if let SIRInstruction::Load(_, _, offset, width) = &mut instructions[0] {
                        *offset = SIROffset::Static(31);
                        *width = 2;
                    }
                }
                _ => unreachable!(),
            }
            assert!(
                recover_independent_stores(&changed, &objects, &Default::default()).is_err(),
                "mutation={mutation}"
            );
        }
    }
}
