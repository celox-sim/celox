//! Experimental SIR adapter for dependence-constrained affine scheduling.
//!
//! A body is one atomic memory statement with explicit iteration provenance.
//! The adapter derives every access from its SIR instructions and proves index
//! arithmetic does not wrap. It never accepts caller-supplied dependence edges.
//! Ordinary optimizer pipelines do not enable this experiment automatically.

// A one-dimensional iteration domain is a vector containing one range.
#![allow(clippy::single_range_in_vec_init)]

mod emit;
mod extract;
mod regions;
mod unrolled;

use crate::{
    BinaryOp, ExecutionUnit, HashMap, RegisterId, RegisterType, SIRInstruction, SIROffset, UnaryOp,
};
use celox_analysis::polyhedral::{
    Access, AccessKind, Affine, Domain, Error, Region, Schedule, ScheduleStats, Statement, Tile,
    scan,
};
use num_traits::{ToPrimitive, Zero};
use std::hash::Hash;

pub use extract::extract;
pub use regions::{RecoveredRegions, RegionOptions, RegionRejection, recover_independent_regions};
pub use unrolled::{UnrolledOptions, recover_independent_stores};
type Result<T> = std::result::Result<T, Error>;

/// Numeric code-generation parameters for an already verified schedule.
#[derive(Clone, Debug)]
pub struct CodegenOptions {
    /// Consecutive points of the innermost scan coordinate per loop iteration.
    pub unroll: usize,
    pub max_instructions: usize,
}

impl Default for CodegenOptions {
    fn default() -> Self {
        Self {
            unroll: 1,
            max_instructions: 100_000,
        }
    }
}

#[derive(Clone, Debug)]
pub struct MemoryObject {
    /// One complete, non-overlapping cell. Distinct map keys must refer to
    /// distinct semantic objects; physical aliasing must not be introduced
    /// after scheduling without revalidating its lifetime requirements.
    pub element_width: usize,
    pub elements: usize,
}

#[derive(Clone, Debug)]
pub struct StatementBody<A> {
    pub domain: Domain,
    pub original_schedule: Vec<Affine>,
    pub induction: Vec<RegisterId>,
    pub register_types: HashMap<RegisterId, RegisterType>,
    pub instructions: Vec<SIRInstruction<A>>,
}

#[derive(Clone, Debug)]
pub struct Kernel<A> {
    region: Region,
    bodies: Vec<StatementBody<A>>,
}

impl<A: Clone + Eq + Hash> Kernel<A> {
    pub fn from_bodies(
        bodies: Vec<StatementBody<A>>,
        objects: &HashMap<A, MemoryObject>,
    ) -> Result<Self> {
        let mut identities = HashMap::default();
        let mut statements = Vec::new();
        for body in &bodies {
            let block = crate::BasicBlock {
                id: crate::BlockId(0),
                params: body.induction.clone(),
                instructions: body.instructions.clone(),
                terminator: crate::SIRTerminator::Return,
            };
            let unit = ExecutionUnit {
                blocks: [(block.id, block)].into_iter().collect(),
                register_map: body.register_types.clone(),
                entry_block_id: crate::BlockId(0),
            };
            unit.verify_result()
                .map_err(|_| Error::Invalid("SIR statement failed verification"))?;
            let d = body.domain.bounds.len();
            if body.induction.len() != d
                || body
                    .domain
                    .constraints
                    .iter()
                    .any(|c| c.coefficients.len() != d)
            {
                return Err(Error::Invalid("SIR iteration provenance"));
            }
            let mut values = HashMap::default();
            let mut defined = crate::HashSet::default();
            for (axis, &register) in body.induction.iter().enumerate() {
                if !defined.insert(register) {
                    return Err(Error::Invalid("duplicate induction register"));
                }
                let value = Affine::axis(d, axis);
                let ty = body
                    .register_types
                    .get(&register)
                    .ok_or(Error::Invalid("induction register type"))?;
                if !matches!(ty, RegisterType::Bit { .. }) || !fits(&value, &body.domain, ty)? {
                    return Err(Error::Invalid("unproved induction range"));
                }
                values.insert(register, Some(value));
            }
            let mut accesses = Vec::new();
            for instruction in &body.instructions {
                let mut missing = false;
                crate::analysis::visit_instruction_uses(instruction, |reg| {
                    missing |= !defined.contains(&reg);
                });
                if missing {
                    return Err(Error::Invalid(
                        "SIR statement has an unmodeled live-in or recurrence",
                    ));
                }
                if let Some(register) = instruction.defined_register() {
                    if !defined.insert(register) || !body.register_types.contains_key(&register) {
                        return Err(Error::Invalid("SIR register definition"));
                    }
                }
                let memory = match instruction {
                    SIRInstruction::Load(_, address, offset, width) => {
                        Some((address, offset, *width, AccessKind::Read))
                    }
                    SIRInstruction::Store(address, offset, width, _, triggers, captures)
                        if triggers.is_empty() && captures.is_empty() =>
                    {
                        Some((address, offset, *width, AccessKind::Write))
                    }
                    SIRInstruction::Imm(..)
                    | SIRInstruction::Unary(..)
                    | SIRInstruction::Concat(..)
                    | SIRInstruction::Slice(..)
                    | SIRInstruction::Mux(..) => None,
                    SIRInstruction::Binary(_, _, op, _)
                        if !matches!(
                            op,
                            BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS
                        ) =>
                    {
                        None
                    }
                    _ => {
                        return Err(Error::Invalid(
                            "SIR statement contains an observable effect or potentially trapping operation",
                        ));
                    }
                };
                if let Some((address, offset, width, kind)) = memory {
                    let object = objects
                        .get(address)
                        .ok_or(Error::Invalid("missing SIR memory shape"))?;
                    if width == 0 || width != object.element_width || object.elements == 0 {
                        return Err(Error::Invalid("partial or overlapping SIR element access"));
                    }
                    let index = element_index(offset, object, &values, d)?;
                    let (low, high) = extent(&index, &body.domain)?;
                    if low < 0 || high >= object.elements as i128 {
                        return Err(Error::Invalid("SIR array access is not proved in bounds"));
                    }
                    let next = identities.len();
                    let object = *identities.entry(address.clone()).or_insert(next);
                    accesses.push(Access {
                        object,
                        subscripts: vec![index],
                        kind,
                    });
                }
                if let Some(register) = instruction.defined_register() {
                    let value =
                        affine_value(instruction, &values, &body.register_types, &body.domain)?;
                    values.insert(register, value);
                }
            }
            statements.push(Statement {
                domain: body.domain.clone(),
                original_schedule: body.original_schedule.clone(),
                accesses,
            });
        }
        Ok(Self {
            region: Region { statements },
            bodies,
        })
    }

    pub fn region(&self) -> &Region {
        &self.region
    }

    pub fn lower(
        &self,
        schedule: &Schedule,
        tile: Option<&Tile>,
        max_work: usize,
    ) -> Result<ExecutionUnit<A>> {
        self.lower_with_options(schedule, tile, &CodegenOptions::default(), max_work)
    }

    pub fn lower_with_options(
        &self,
        schedule: &Schedule,
        tile: Option<&Tile>,
        options: &CodegenOptions,
        max_work: usize,
    ) -> Result<ExecutionUnit<A>> {
        if !(1..=64).contains(&options.unroll) {
            return Err(Error::Invalid("affine unroll factor must be in 1..=64"));
        }
        let instructions = self
            .bodies
            .iter()
            .map(|b| b.instructions.len())
            .sum::<usize>();
        if instructions
            .checked_mul(options.unroll)
            .is_none_or(|n| n > options.max_instructions)
        {
            return Err(Error::WorkLimit);
        }
        let plan = scan(&self.region, schedule, tile, max_work)?;
        emit::lower(&self.bodies, &self.region, &plan, options)
    }

    pub fn lower_original(&self, max_work: usize) -> Result<ExecutionUnit<A>> {
        let dimensions = self
            .region
            .statements
            .first()
            .ok_or(Error::Invalid("empty SIR kernel"))?
            .original_schedule
            .len();
        let original = Schedule {
            rows: (0..dimensions)
                .map(|row| {
                    self.region
                        .statements
                        .iter()
                        .map(|s| s.original_schedule[row].clone())
                        .collect()
                })
                .collect(),
            bands: Vec::new(),
            distances: Vec::new(),
            stats: ScheduleStats::default(),
        };
        self.lower(&original, None, max_work)
    }
}

fn extent(expression: &Affine, domain: &Domain) -> Result<(i128, i128)> {
    let mut low = i128::from(expression.constant);
    let mut high = low;
    for (&coefficient, range) in expression.coefficients.iter().zip(&domain.bounds) {
        if range.start >= range.end {
            return Err(Error::Invalid("empty SIR loop domain"));
        }
        let (a, b) = if coefficient >= 0 {
            (range.start, range.end - 1)
        } else {
            (range.end - 1, range.start)
        };
        low = low
            .checked_add(i128::from(coefficient) * i128::from(a))
            .ok_or(Error::ArithmeticOverflow)?;
        high = high
            .checked_add(i128::from(coefficient) * i128::from(b))
            .ok_or(Error::ArithmeticOverflow)?;
    }
    Ok((low, high))
}

fn fits(expression: &Affine, domain: &Domain, ty: &RegisterType) -> Result<bool> {
    let width = ty.width();
    if width == 0 || width > 64 {
        return Ok(false);
    }
    let (low, high) = extent(expression, domain)?;
    let (minimum, maximum) = if ty.is_signed() {
        (-(1i128 << (width - 1)), (1i128 << (width - 1)) - 1)
    } else {
        (0, (1i128 << width) - 1)
    };
    Ok(low >= minimum && high <= maximum)
}

fn combine(a: &Affine, b: &Affine, scale: i64) -> Result<Affine> {
    let convert = |x: i64, y: i64| {
        i64::try_from(i128::from(x) + i128::from(y) * i128::from(scale))
            .map_err(|_| Error::ArithmeticOverflow)
    };
    Ok(Affine::new(
        a.coefficients
            .iter()
            .zip(&b.coefficients)
            .map(|(&x, &y)| convert(x, y))
            .collect::<Result<Vec<_>>>()?,
        convert(a.constant, b.constant)?,
    ))
}

fn affine_value<A>(
    instruction: &SIRInstruction<A>,
    values: &HashMap<RegisterId, Option<Affine>>,
    types: &HashMap<RegisterId, RegisterType>,
    domain: &Domain,
) -> Result<Option<Affine>> {
    let d = domain.bounds.len();
    let Some(register) = instruction.defined_register() else {
        return Ok(None);
    };
    let ty = &types[&register];
    let value = match instruction {
        SIRInstruction::Imm(_, value)
            if value.mask.is_zero() && ty.width() > 0 && ty.width() <= 64 =>
        {
            let Some(payload) = value.payload.to_u64() else {
                return Ok(None);
            };
            let width = ty.width();
            let payload = i128::from(payload) & ((1i128 << width) - 1);
            let signed = if ty.is_signed() && payload & (1i128 << (width - 1)) != 0 {
                payload - (1i128 << width)
            } else {
                payload
            };
            let Ok(value) = i64::try_from(signed) else {
                return Ok(None);
            };
            Some(Affine::constant(d, value))
        }
        SIRInstruction::Binary(_, left, op, right)
            if types[left].width() == ty.width() && types[right].width() == ty.width() =>
        {
            let (Some(a), Some(b)) = (&values[left], &values[right]) else {
                return Ok(None);
            };
            match op {
                BinaryOp::Add => Some(combine(a, b, 1)?),
                BinaryOp::Sub => Some(combine(a, b, -1)?),
                BinaryOp::Mul if b.coefficients.iter().all(|&v| v == 0) => {
                    Some(combine(&Affine::constant(d, 0), a, b.constant)?)
                }
                BinaryOp::Mul if a.coefficients.iter().all(|&v| v == 0) => {
                    Some(combine(&Affine::constant(d, 0), b, a.constant)?)
                }
                BinaryOp::Shl
                    if b.coefficients.iter().all(|&v| v == 0) && (0..63).contains(&b.constant) =>
                {
                    Some(combine(&Affine::constant(d, 0), a, 1i64 << b.constant)?)
                }
                _ => None,
            }
        }
        SIRInstruction::Unary(_, UnaryOp::Ident | UnaryOp::ToTwoState, source)
        | SIRInstruction::Slice(_, source, 0, _) => values[source].clone(),
        _ => None,
    };
    if let Some(value) = value
        && fits(&value, domain, ty)?
    {
        Ok(Some(value))
    } else {
        Ok(None)
    }
}

fn element_index(
    offset: &SIROffset,
    object: &MemoryObject,
    values: &HashMap<RegisterId, Option<Affine>>,
    dimensions: usize,
) -> Result<Affine> {
    let failure = || Error::Invalid("SIR memory index is non-affine or may wrap");
    match offset {
        SIROffset::Element {
            index,
            element_width,
            bit_offset: 0,
            dynamic_bit_offset: None,
        } if *element_width == object.element_width => {
            values.get(index).and_then(Clone::clone).ok_or_else(failure)
        }
        SIROffset::Static(offset) if offset % object.element_width == 0 => Ok(Affine::constant(
            dimensions,
            i64::try_from(offset / object.element_width).map_err(|_| Error::ArithmeticOverflow)?,
        )),
        SIROffset::PackedElements {
            bit_offset,
            element_width,
        } if *element_width == object.element_width && bit_offset % object.element_width == 0 => {
            Ok(Affine::constant(
                dimensions,
                i64::try_from(bit_offset / object.element_width)
                    .map_err(|_| Error::ArithmeticOverflow)?,
            ))
        }
        SIROffset::Dynamic(index) => {
            let mut expression = values
                .get(index)
                .and_then(Clone::clone)
                .ok_or_else(failure)?;
            let divisor =
                i64::try_from(object.element_width).map_err(|_| Error::ArithmeticOverflow)?;
            if expression
                .coefficients
                .iter()
                .chain([&expression.constant])
                .any(|value| value % divisor != 0)
            {
                return Err(failure());
            }
            for coefficient in &mut expression.coefficients {
                *coefficient /= divisor;
            }
            expression.constant /= divisor;
            Ok(expression)
        }
        _ => Err(failure()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SIRValue;

    fn sample(offset: u8) -> (StatementBody<u32>, HashMap<u32, MemoryObject>) {
        let (i, constant, index, value) =
            (RegisterId(0), RegisterId(1), RegisterId(2), RegisterId(3));
        let body = StatementBody {
            domain: Domain::rectangular(vec![0..4]),
            original_schedule: vec![Affine::axis(1, 0)],
            induction: vec![i],
            register_types: [
                (
                    i,
                    RegisterType::Bit {
                        width: 8,
                        signed: true,
                    },
                ),
                (
                    constant,
                    RegisterType::Bit {
                        width: 8,
                        signed: true,
                    },
                ),
                (
                    index,
                    RegisterType::Bit {
                        width: 8,
                        signed: true,
                    },
                ),
                (
                    value,
                    RegisterType::Bit {
                        width: 64,
                        signed: false,
                    },
                ),
            ]
            .into_iter()
            .collect(),
            instructions: vec![
                SIRInstruction::Imm(constant, SIRValue::new(offset)),
                SIRInstruction::Binary(index, i, BinaryOp::Add, constant),
                SIRInstruction::Load(
                    value,
                    0,
                    SIROffset::Element {
                        index,
                        element_width: 64,
                        bit_offset: 0,
                        dynamic_bit_offset: None,
                    },
                    64,
                ),
                SIRInstruction::Store(
                    1,
                    SIROffset::Element {
                        index,
                        element_width: 64,
                        bit_offset: 0,
                        dynamic_bit_offset: None,
                    },
                    64,
                    value,
                    vec![],
                    vec![],
                ),
            ],
        };
        (
            body,
            (0..2)
                .map(|a| {
                    (
                        a,
                        MemoryObject {
                            element_width: 64,
                            elements: 256,
                        },
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn access_proof_distinguishes_integer_indices_from_bit_wraparound() {
        let (body, objects) = sample(124);
        let kernel = Kernel::from_bodies(vec![body], &objects).unwrap();
        assert_eq!(
            kernel.region().statements[0].accesses[0].subscripts,
            vec![Affine::new(vec![1], 124)]
        );
        let (body, objects) = sample(125);
        assert!(matches!(
            Kernel::from_bodies(vec![body], &objects),
            Err(Error::Invalid("SIR memory index is non-affine or may wrap"))
        ));
    }

    #[test]
    fn unknown_shapes_partial_cells_and_wrong_packing_are_rejected() {
        let (body, mut objects) = sample(0);
        objects.remove(&0);
        assert!(Kernel::from_bodies(vec![body.clone()], &objects).is_err());
        objects.insert(
            0,
            MemoryObject {
                element_width: 32,
                elements: 512,
            },
        );
        assert!(Kernel::from_bodies(vec![body.clone()], &objects).is_err());
        objects.insert(
            0,
            MemoryObject {
                element_width: 64,
                elements: 2,
            },
        );
        assert!(Kernel::from_bodies(vec![body], &objects).is_err());
        let (mut body, objects) = sample(0);
        let SIRInstruction::Load(_, _, offset, _) = &mut body.instructions[2] else {
            unreachable!()
        };
        *offset = SIROffset::PackedElements {
            bit_offset: 0,
            element_width: 32,
        };
        assert!(Kernel::from_bodies(vec![body], &objects).is_err());
    }

    #[test]
    fn effects_potential_traps_and_unmodeled_live_ins_are_rejected() {
        let (body, objects) = sample(0);
        let mut captured = body.clone();
        let SIRInstruction::Store(_, _, _, _, _, captures) = &mut captured.instructions[3] else {
            unreachable!()
        };
        captures.push(0);
        assert!(Kernel::from_bodies(vec![captured], &objects).is_err());
        let mut event = body.clone();
        event.instructions.push(SIRInstruction::RuntimeEvent {
            site_id: 0,
            args: vec![RegisterId(3)],
        });
        assert!(Kernel::from_bodies(vec![event], &objects).is_err());
        let mut trap = body.clone();
        let SIRInstruction::Binary(_, _, op, _) = &mut trap.instructions[1] else {
            unreachable!()
        };
        *op = BinaryOp::DivS;
        assert!(Kernel::from_bodies(vec![trap], &objects).is_err());
        let mut live_in = body;
        live_in.instructions.remove(0);
        assert!(Kernel::from_bodies(vec![live_in], &objects).is_err());
    }

    #[test]
    fn malformed_ssa_is_an_error_instead_of_a_panic() {
        let (mut body, objects) = sample(0);
        body.register_types.remove(&RegisterId(3));
        assert!(Kernel::from_bodies(vec![body], &objects).is_err());
    }

    #[test]
    fn code_generation_parameters_and_size_are_bounded() {
        let (body, objects) = sample(0);
        let kernel = Kernel::from_bodies(vec![body], &objects).unwrap();
        let schedule =
            celox_analysis::polyhedral::schedule(kernel.region(), &Default::default()).unwrap();
        for unroll in [0, 65] {
            let options = CodegenOptions {
                unroll,
                ..Default::default()
            };
            assert!(matches!(
                kernel.lower_with_options(&schedule, None, &options, 2_000_000),
                Err(Error::Invalid(_))
            ));
        }
        let options = CodegenOptions {
            unroll: 2,
            max_instructions: 1,
        };
        assert!(matches!(
            kernel.lower_with_options(&schedule, None, &options, 2_000_000),
            Err(Error::WorkLimit)
        ));
    }
}
