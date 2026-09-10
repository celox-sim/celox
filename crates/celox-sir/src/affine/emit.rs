use super::{CodegenOptions, Result, StatementBody};
use crate::{
    BinaryOp, BlockId, ExecutionUnit, HashMap, RegisterId, RegisterType, SIRBuilder,
    SIRInstruction, SIROffset, SIRTerminator, SIRValue, UnaryOp,
};
use celox_analysis::polyhedral::{Access, Affine, Bound, Error, Region, ScanPlan};
use std::{hash::Hash, ops::Deref};

#[derive(Clone, Copy)]
struct Value {
    register: RegisterId,
    constant: Option<i64>,
    /// Exact base + displacement for generated integer coordinates.
    origin: Option<(RegisterId, i64)>,
}

#[derive(Clone)]
struct Body<'a, A> {
    source: &'a StatementBody<A>,
    accesses: &'a [Access],
}

impl<A> Deref for Body<'_, A> {
    type Target = StatementBody<A>;
    fn deref(&self) -> &Self::Target {
        self.source
    }
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct IndexKey {
    terms: Vec<(RegisterId, i64)>,
    constant: i64,
}

/// One straight-line batch. Canonical integer addresses expose equal loads
/// across unrolled lanes; typed value numbering retains their expression DAG.
/// A store invalidates every cached load of its object, including other cells.
/// No availability crosses a loop backedge or a control-flow boundary.
struct Sharing<A> {
    indices: HashMap<IndexKey, RegisterId>,
    values: HashMap<(RegisterType, SIRInstruction<A>), RegisterId>,
}

impl<A: Clone + Eq + Hash> Sharing<A> {
    fn new() -> Self {
        Self {
            indices: HashMap::default(),
            values: HashMap::default(),
        }
    }

    fn index(
        &mut self,
        builder: &mut SIRBuilder<A>,
        source: &Affine,
        values: &[Value],
    ) -> Result<RegisterId> {
        let mut terms = HashMap::<RegisterId, i128>::default();
        let mut constant = i128::from(source.constant);
        for (&coefficient, value) in source.coefficients.iter().zip(values) {
            if coefficient == 0 {
                continue;
            }
            let displacement = if let Some(c) = value.constant {
                c
            } else {
                let (base, offset) = value.origin.unwrap_or((value.register, 0));
                let term = terms.entry(base).or_default();
                *term = term
                    .checked_add(i128::from(coefficient))
                    .ok_or(Error::ArithmeticOverflow)?;
                offset
            };
            constant = constant
                .checked_add(i128::from(coefficient) * i128::from(displacement))
                .ok_or(Error::ArithmeticOverflow)?;
        }
        let mut terms = terms
            .into_iter()
            .filter(|(_, c)| *c != 0)
            .map(|(r, c)| Ok((r, i64::try_from(c).map_err(|_| Error::ArithmeticOverflow)?)))
            .collect::<Result<Vec<_>>>()?;
        terms.sort_unstable();
        let key = IndexKey {
            terms,
            constant: i64::try_from(constant).map_err(|_| Error::ArithmeticOverflow)?,
        };
        if let Some(&register) = self.indices.get(&key) {
            return Ok(register);
        }
        let affine = Affine::new(
            key.terms.iter().map(|(_, c)| *c).collect::<Vec<_>>(),
            key.constant,
        );
        let coordinates = key
            .terms
            .iter()
            .map(|&(register, _)| Value {
                register,
                constant: None,
                origin: Some((register, 0)),
            })
            .collect::<Vec<_>>();
        let value = expression(builder, &affine, &coordinates)?;
        self.indices.insert(key, value.register);
        Ok(value.register)
    }

    fn emit(
        &mut self,
        builder: &mut SIRBuilder<A>,
        instruction: SIRInstruction<A>,
    ) -> Option<RegisterId> {
        if let SIRInstruction::Store(address, ..) = &instruction {
            self.values.retain(|(_, key), _| !matches!(key, SIRInstruction::Load(_, loaded, ..) if loaded == address));
        }
        let destination = instruction.defined_register();
        if let Some(destination) = destination {
            let mut key = instruction.clone();
            match &mut key {
                SIRInstruction::Imm(dst, _)
                | SIRInstruction::Binary(dst, ..)
                | SIRInstruction::Unary(dst, ..)
                | SIRInstruction::Load(dst, ..)
                | SIRInstruction::Concat(dst, ..)
                | SIRInstruction::Slice(dst, ..)
                | SIRInstruction::Mux(dst, ..) => *dst = RegisterId(0),
                _ => unreachable!("only validated statement instructions are emitted"),
            }
            let key = (builder.register(&destination).clone(), key);
            if let Some(&canonical) = self.values.get(&key) {
                return Some(canonical);
            }
            self.values.insert(key, destination);
        }
        builder.emit(instruction);
        destination
    }
}

fn immediate<A>(builder: &mut SIRBuilder<A>, value: i64) -> Value {
    let register = builder.alloc_bit(64, true);
    builder.emit(SIRInstruction::Imm(register, SIRValue::new(value as u64)));
    Value {
        register,
        constant: Some(value),
        origin: None,
    }
}

fn binary<A>(
    builder: &mut SIRBuilder<A>,
    left: Value,
    op: BinaryOp,
    right: Value,
) -> Result<Value> {
    if let (Some(a), Some(b)) = (left.constant, right.constant) {
        let value = match op {
            BinaryOp::Add => a.checked_add(b),
            BinaryOp::Sub => a.checked_sub(b),
            BinaryOp::Mul => a.checked_mul(b),
            BinaryOp::DivS => a.checked_div(b),
            BinaryOp::RemS => a.checked_rem(b),
            BinaryOp::LtS => Some(i64::from(a < b)),
            BinaryOp::LeS => Some(i64::from(a <= b)),
            BinaryOp::GeS => Some(i64::from(a >= b)),
            BinaryOp::GtS => Some(i64::from(a > b)),
            BinaryOp::And => Some(a & b),
            _ => return Err(Error::Invalid("scan arithmetic opcode")),
        }
        .ok_or(Error::ArithmeticOverflow)?;
        return Ok(immediate(builder, value));
    }
    let comparison = matches!(
        op,
        BinaryOp::LtS | BinaryOp::LeS | BinaryOp::GeS | BinaryOp::GtS
    );
    let register = builder.alloc_bit(if comparison { 1 } else { 64 }, !comparison);
    builder.emit(SIRInstruction::Binary(
        register,
        left.register,
        op,
        right.register,
    ));
    let translate = |value: Value, delta: i64| -> Result<_> {
        let (base, offset) = value.origin.unwrap_or((value.register, 0));
        Ok(Some((
            base,
            offset.checked_add(delta).ok_or(Error::ArithmeticOverflow)?,
        )))
    };
    let origin = match (op, left.constant, right.constant) {
        (BinaryOp::Add, None, Some(c)) => translate(left, c)?,
        (BinaryOp::Add, Some(c), None) => translate(right, c)?,
        (BinaryOp::Sub, None, Some(c)) => {
            translate(left, c.checked_neg().ok_or(Error::ArithmeticOverflow)?)?
        }
        _ => None,
    };
    Ok(Value {
        register,
        constant: None,
        origin,
    })
}

fn select<A>(builder: &mut SIRBuilder<A>, condition: Value, yes: Value, no: Value) -> Value {
    if let Some(value) = condition.constant {
        return if value != 0 { yes } else { no };
    }
    let widen = |builder: &mut SIRBuilder<A>, value: Value| {
        if builder.register(&value.register).width() == 64 {
            return value;
        }
        let register = builder.alloc_bit(64, true);
        builder.emit(SIRInstruction::Unary(
            register,
            UnaryOp::Ident,
            value.register,
        ));
        Value {
            register,
            constant: value.constant,
            origin: value.origin,
        }
    };
    let yes = widen(builder, yes);
    let no = widen(builder, no);
    let register = builder.alloc_bit(64, true);
    builder.emit(SIRInstruction::Mux(
        register,
        condition.register,
        yes.register,
        no.register,
    ));
    Value {
        register,
        constant: None,
        origin: None,
    }
}

fn expression<A>(
    builder: &mut SIRBuilder<A>,
    affine: &Affine,
    coordinates: &[Value],
) -> Result<Value> {
    let mut value = None;
    let mut constant = affine.constant;
    for (&coefficient, &coordinate) in affine.coefficients.iter().zip(coordinates) {
        if coefficient == 0 {
            continue;
        }
        if let Some(c) = coordinate.constant {
            constant =
                i64::try_from(i128::from(constant) + i128::from(coefficient) * i128::from(c))
                    .map_err(|_| Error::ArithmeticOverflow)?;
            continue;
        }
        let term = if coefficient == 1 {
            coordinate
        } else {
            let coefficient = immediate(builder, coefficient);
            binary(builder, coordinate, BinaryOp::Mul, coefficient)?
        };
        value = Some(if let Some(previous) = value {
            binary(builder, previous, BinaryOp::Add, term)?
        } else {
            term
        });
    }
    match value {
        Some(value) if constant == 0 => Ok(value),
        Some(value) => {
            let constant = immediate(builder, constant);
            binary(builder, value, BinaryOp::Add, constant)
        }
        None => Ok(immediate(builder, constant)),
    }
}

fn bound<A>(
    builder: &mut SIRBuilder<A>,
    bound: &Bound,
    prefix: &[Value],
    lower: bool,
) -> Result<Value> {
    let numerator = expression(builder, &bound.numerator, prefix)?;
    if bound.denominator == 1 {
        return Ok(numerator);
    }
    let denominator = immediate(builder, bound.denominator);
    let quotient = binary(builder, numerator, BinaryOp::DivS, denominator)?;
    let remainder = binary(builder, numerator, BinaryOp::RemS, denominator)?;
    let zero = immediate(builder, 0);
    let condition = binary(
        builder,
        remainder,
        if lower { BinaryOp::GtS } else { BinaryOp::LtS },
        zero,
    )?;
    let one = immediate(builder, 1);
    let adjustment = select(builder, condition, one, zero);
    binary(
        builder,
        quotient,
        if lower { BinaryOp::Add } else { BinaryOp::Sub },
        adjustment,
    )
}

fn extremum<A>(builder: &mut SIRBuilder<A>, values: &[Value], minimum: bool) -> Result<Value> {
    let mut value = *values.first().ok_or(Error::Invalid("empty scan bound"))?;
    for &next in &values[1..] {
        let condition = binary(
            builder,
            next,
            if minimum {
                BinaryOp::LtS
            } else {
                BinaryOp::GtS
            },
            value,
        )?;
        value = select(builder, condition, next, value);
    }
    Ok(value)
}

fn union_bound<A>(
    builder: &mut SIRBuilder<A>,
    bounds: &[Vec<Bound>],
    prefix: &[Value],
    lower: bool,
) -> Result<Value> {
    let mut statements = Vec::new();
    for bounds in bounds {
        let values = bounds
            .iter()
            .map(|b| bound(builder, b, prefix, lower))
            .collect::<Result<Vec<_>>>()?;
        statements.push(extremum(builder, &values, !lower)?);
    }
    extremum(builder, &statements, lower)
}

fn guarded<A>(
    builder: &mut SIRBuilder<A>,
    condition: Value,
    emit: impl FnOnce(&mut SIRBuilder<A>) -> Result<()>,
) -> Result<()> {
    if let Some(value) = condition.constant {
        if value != 0 {
            emit(builder)?;
        }
        return Ok(());
    }
    let yes = builder.new_block();
    let exit = builder.new_block();
    builder.seal_block(SIRTerminator::Branch {
        cond: condition.register,
        true_block: (yes, vec![]),
        false_block: (exit, vec![]),
    });
    builder.switch_to_block(yes);
    emit(builder)?;
    builder.seal_block(SIRTerminator::Jump(exit, vec![]));
    builder.switch_to_block(exit);
    Ok(())
}

fn offset(offset: &SIROffset, registers: &HashMap<RegisterId, RegisterId>) -> SIROffset {
    match offset {
        SIROffset::Static(_) | SIROffset::PackedElements { .. } => offset.clone(),
        SIROffset::Dynamic(index) => SIROffset::Dynamic(registers[index]),
        SIROffset::Element {
            index,
            element_width,
            bit_offset,
            dynamic_bit_offset,
        } => SIROffset::Element {
            index: registers[index],
            element_width: *element_width,
            bit_offset: *bit_offset,
            dynamic_bit_offset: dynamic_bit_offset.map(|r| registers[&r]),
        },
    }
}

fn body<A: Clone + Eq + Hash>(
    builder: &mut SIRBuilder<A>,
    source: &Body<'_, A>,
    coordinates: &[Affine],
    prefix: &[Value],
) -> Result<()> {
    let values = coordinates
        .iter()
        .map(|coordinate| expression(builder, coordinate, prefix))
        .collect::<Result<Vec<_>>>()?;
    body_values(builder, source, &values, None)
}

fn body_values<A: Clone + Eq + Hash>(
    builder: &mut SIRBuilder<A>,
    source: &Body<'_, A>,
    values: &[Value],
    mut shared: Option<&mut Sharing<A>>,
) -> Result<()> {
    let mut registers = HashMap::default();
    let mut access_index = 0;
    for (&old, value) in source.induction.iter().zip(values) {
        let new = builder.alloc_reg(source.register_types[&old].clone());
        builder.emit(SIRInstruction::Unary(new, UnaryOp::Ident, value.register));
        registers.insert(old, new);
    }
    for instruction in &source.instructions {
        let old_destination = instruction.defined_register();
        if let Some(old) = instruction.defined_register() {
            registers.insert(old, builder.alloc_reg(source.register_types[&old].clone()));
        }
        let r = |reg: &RegisterId| registers[reg];
        let mut instruction = match instruction {
            SIRInstruction::Imm(dst, value) => SIRInstruction::Imm(r(dst), value.clone()),
            SIRInstruction::Binary(dst, left, op, right) => {
                SIRInstruction::Binary(r(dst), r(left), *op, r(right))
            }
            SIRInstruction::Unary(dst, op, source) => SIRInstruction::Unary(r(dst), *op, r(source)),
            SIRInstruction::Load(dst, address, at, width) => {
                SIRInstruction::Load(r(dst), address.clone(), offset(at, &registers), *width)
            }
            SIRInstruction::Store(address, at, width, value, triggers, captures) => {
                SIRInstruction::Store(
                    address.clone(),
                    offset(at, &registers),
                    *width,
                    r(value),
                    triggers.clone(),
                    captures.clone(),
                )
            }
            SIRInstruction::Concat(dst, sources) => {
                SIRInstruction::Concat(r(dst), sources.iter().map(r).collect())
            }
            SIRInstruction::Slice(dst, source, start, width) => {
                SIRInstruction::Slice(r(dst), r(source), *start, *width)
            }
            SIRInstruction::Mux(dst, cond, yes, no) => {
                SIRInstruction::Mux(r(dst), r(cond), r(yes), r(no))
            }
            _ => return Err(Error::Invalid("unvalidated SIR body")),
        };
        if matches!(
            instruction,
            SIRInstruction::Load(..) | SIRInstruction::Store(..)
        ) {
            if let Some(shared) = shared.as_mut() {
                let expression = &source.accesses[access_index].subscripts[0];
                let index = shared.index(builder, expression, values)?;
                match &mut instruction {
                    SIRInstruction::Load(_, _, at, width)
                    | SIRInstruction::Store(_, at, width, ..) => {
                        *at = SIROffset::Element {
                            index,
                            element_width: *width,
                            bit_offset: 0,
                            dynamic_bit_offset: None,
                        };
                    }
                    _ => unreachable!(),
                }
            }
            access_index += 1;
        }
        if let Some(shared) = shared.as_mut() {
            let canonical = shared.emit(builder, instruction);
            if let (Some(old), Some(canonical)) = (old_destination, canonical) {
                registers.insert(old, canonical);
            }
        } else {
            builder.emit(instruction);
        }
    }
    Ok(())
}

fn suffix(row: &Affine, fixed: &[Option<i64>], axis: usize) -> Result<Affine> {
    let mut result = Affine::new(row.coefficients[..axis].to_vec(), row.constant);
    for (&coefficient, value) in row.coefficients[axis + 1..].iter().zip(&fixed[axis + 1..]) {
        result.constant = i64::try_from(
            i128::from(result.constant) + i128::from(coefficient) * i128::from(value.unwrap()),
        )
        .map_err(|_| Error::ArithmeticOverflow)?;
    }
    Ok(result)
}

fn magnitude(row: &Affine, bounds: &[(i64, i64)]) -> Result<i128> {
    let mut value = i128::from(row.constant).abs();
    for (&coefficient, &(low, high)) in row.coefficients.iter().zip(bounds) {
        value = value
            .checked_add(
                i128::from(coefficient)
                    .abs()
                    .checked_mul(i128::from(low).abs().max(i128::from(high).abs()))
                    .ok_or(Error::ArithmeticOverflow)?,
            )
            .ok_or(Error::ArithmeticOverflow)?;
    }
    Ok(value)
}

/// Choose a source iterator as the interior loop counter when it has unit
/// slope. Compute all inverse-map translations in the preheader. In a skewed
/// nest this emits an ordinary spatial induction variable, rather than
/// repeatedly reconstructing i = x - 2*t inside every statement instance.
fn interior_loop<A: Clone + Eq + Hash>(
    builder: &mut SIRBuilder<A>,
    plan: &ScanPlan,
    bodies: &[Body<'_, A>],
    prefix: &[Value],
    start: Value,
    end: Value,
    options: &CodegenOptions,
) -> Result<()> {
    let axis = prefix.len();
    let mut translation = Affine::constant(axis, 0);
    'found: for statement in &plan.statements {
        for row in &statement.iterators {
            if row.coefficients[axis] == 1 {
                translation = suffix(row, &statement.fixed_coordinates, axis)?;
                break 'found;
            }
        }
    }
    let shifted_magnitude = magnitude(&translation, &plan.coordinate_bounds)?
        .checked_add(
            i128::from(plan.coordinate_bounds[axis].0)
                .abs()
                .max(i128::from(plan.coordinate_bounds[axis].1).abs())
                + options.unroll as i128
                + 2,
        )
        .ok_or(Error::ArithmeticOverflow)?;
    if shifted_magnitude >= i128::from(i64::MAX) {
        return Err(Error::ArithmeticOverflow);
    }
    let shift = expression(builder, &translation, prefix)?;
    let start = binary(builder, start, BinaryOp::Add, shift)?;
    let end = binary(builder, end, BinaryOp::Add, shift)?;
    let mut prepared = Vec::new();
    for statement in &plan.statements {
        let mut iterators = Vec::new();
        for row in &statement.iterators {
            let coefficient = row.coefficients[axis];
            let intercept = super::combine(
                &suffix(row, &statement.fixed_coordinates, axis)?,
                &translation,
                coefficient.checked_neg().ok_or(Error::ArithmeticOverflow)?,
            )?;
            let extent = magnitude(&intercept, &plan.coordinate_bounds)?
                .checked_add(
                    i128::from(coefficient)
                        .abs()
                        .checked_mul(shifted_magnitude)
                        .ok_or(Error::ArithmeticOverflow)?,
                )
                .ok_or(Error::ArithmeticOverflow)?;
            if extent >= i128::from(i64::MAX) {
                return Err(Error::ArithmeticOverflow);
            }
            iterators.push((coefficient, expression(builder, &intercept, prefix)?));
        }
        prepared.push(iterators);
    }
    let mut order = (0..bodies.len()).collect::<Vec<_>>();
    order.sort_by_key(|&s| plan.statements[s].fixed_coordinates[axis + 1..].to_vec());
    chunk_loop(builder, start, end, options.unroll, |builder, points| {
        let mut shared = (options.unroll > 1).then(Sharing::new);
        for &value in points {
            for &s in &order {
                let mut iterators = Vec::new();
                for &(coefficient, intercept) in &prepared[s] {
                    let value = match coefficient {
                        0 => intercept,
                        1 if intercept.constant == Some(0) => value,
                        1 => binary(builder, value, BinaryOp::Add, intercept)?,
                        _ => {
                            let coefficient = immediate(builder, coefficient);
                            let product = binary(builder, value, BinaryOp::Mul, coefficient)?;
                            binary(builder, product, BinaryOp::Add, intercept)?
                        }
                    };
                    iterators.push(value);
                }
                body_values(builder, &bodies[s], &iterators, shared.as_mut())?;
            }
        }
        Ok(())
    })
}

fn point_loop<A>(
    builder: &mut SIRBuilder<A>,
    lower: Value,
    upper: Value,
    emit: impl FnOnce(&mut SIRBuilder<A>, Value) -> Result<()>,
) -> Result<()> {
    if lower.constant.is_some() && lower.constant == upper.constant {
        return emit(builder, lower);
    }
    stepped_loop(builder, lower, upper, 1, emit).map(|_| ())
}

fn stepped_loop<A>(
    builder: &mut SIRBuilder<A>,
    lower: Value,
    upper: Value,
    step: i64,
    emit: impl FnOnce(&mut SIRBuilder<A>, Value) -> Result<()>,
) -> Result<Value> {
    if let (Some(lower_value), Some(upper_value)) = (lower.constant, upper.constant)
        && lower_value > upper_value
    {
        return Ok(lower);
    }
    let induction = builder.alloc_bit(64, true);
    let header = builder.new_block_with(vec![induction]);
    let enter = builder.new_block();
    let exit = builder.new_block();
    builder.seal_block(SIRTerminator::Jump(header, vec![lower.register]));
    builder.switch_to_block(header);
    let value = Value {
        register: induction,
        constant: None,
        origin: Some((induction, 0)),
    };
    let condition = binary(builder, value, BinaryOp::LeS, upper)?;
    builder.seal_block(SIRTerminator::Branch {
        cond: condition.register,
        true_block: (enter, vec![]),
        false_block: (exit, vec![]),
    });
    builder.switch_to_block(enter);
    emit(builder, value)?;
    let step = immediate(builder, step);
    let next = binary(builder, value, BinaryOp::Add, step)?;
    builder.seal_block(SIRTerminator::Jump(header, vec![next.register]));
    builder.switch_to_block(exit);
    Ok(value)
}

/// Preserve lexicographic point order, emitting one contiguous batch at a time.
/// Full batches never speculate beyond the proven interval; a separate tail
/// handles the remainder. The caller has proved room for the cutoff arithmetic.
fn chunk_loop<A>(
    builder: &mut SIRBuilder<A>,
    lower: Value,
    upper: Value,
    factor: usize,
    mut emit: impl FnMut(&mut SIRBuilder<A>, &[Value]) -> Result<()>,
) -> Result<()> {
    if factor == 1 {
        return point_loop(builder, lower, upper, |builder, v| emit(builder, &[v]));
    }
    let static_range = lower.constant.zip(upper.constant);
    if let Some((start, end)) = static_range {
        let length = i128::from(end) - i128::from(start) + 1;
        if length <= 0 {
            return Ok(());
        }
        if length <= factor as i128 {
            let values = (start..=end)
                .map(|i| immediate(builder, i))
                .collect::<Vec<_>>();
            return emit(builder, &values);
        }
    }
    let adjustment = immediate(builder, factor as i64 - 1);
    let cutoff = binary(builder, upper, BinaryOp::Sub, adjustment)?;
    let next = stepped_loop(builder, lower, cutoff, factor as i64, |builder, base| {
        let mut values = Vec::new();
        for lane in 0..factor {
            let offset = immediate(builder, lane as i64);
            values.push(binary(builder, base, BinaryOp::Add, offset)?);
        }
        emit(builder, &values)
    })?;
    if let Some((start, end)) = static_range {
        let length = i128::from(end) - i128::from(start) + 1;
        let remainder = (length % factor as i128) as i64;
        if remainder > 0 {
            let first = end - (remainder - 1);
            let values = (first..=end)
                .map(|i| immediate(builder, i))
                .collect::<Vec<_>>();
            emit(builder, &values)?;
        }
        Ok(())
    } else {
        point_loop(builder, next, upper, |builder, v| emit(builder, &[v]))
    }
}

/// Partition a statically bounded innermost coordinate at every statement's
/// endpoints. Boundary-only statements must not make the intersection of all
/// domains empty and force guards on every iteration of a large interior.
/// There are at most twice as many pieces as statements, independent of the
/// trip count. Dynamic-prefix domains continue to use the general scanner.
fn static_pieces<A: Clone + Eq + Hash>(
    builder: &mut SIRBuilder<A>,
    plan: &ScanPlan,
    bodies: &[Body<'_, A>],
    prefix: &[Value],
    lower: Value,
    upper: Value,
    options: &CodegenOptions,
) -> Result<bool> {
    let (Some(lower), Some(upper)) = (lower.constant, upper.constant) else {
        return Ok(false);
    };
    let Some(prefix_values) = prefix
        .iter()
        .map(|v| v.constant)
        .collect::<Option<Vec<_>>>()
    else {
        return Ok(false);
    };
    let axis = prefix.len();
    let mut intervals = Vec::new();
    let mut endpoints = std::collections::BTreeSet::new();
    for statement in &plan.statements {
        let mut coordinates = prefix_values.clone();
        coordinates.push(0);
        coordinates.extend(
            statement.fixed_coordinates[axis + 1..]
                .iter()
                .map(|v| v.unwrap()),
        );
        let (mut low, mut high) = (i128::from(lower), i128::from(upper));
        for row in &statement.domain {
            let constant = row.coefficients.iter().zip(&coordinates).try_fold(
                i128::from(row.constant),
                |sum, (&a, &x)| {
                    sum.checked_add(i128::from(a) * i128::from(x))
                        .ok_or(Error::ArithmeticOverflow)
                },
            )?;
            let coefficient = i128::from(row.coefficients[axis]);
            if coefficient > 0 {
                let numerator = constant.checked_neg().ok_or(Error::ArithmeticOverflow)?;
                let bound = numerator.div_euclid(coefficient)
                    + i128::from(numerator.rem_euclid(coefficient) != 0);
                low = low.max(bound);
            } else if coefficient < 0 {
                high = high.min(constant.div_euclid(-coefficient));
            } else if constant < 0 {
                low = 1;
                high = 0;
                break;
            }
        }
        if low <= high {
            let start = i64::try_from(low).map_err(|_| Error::ArithmeticOverflow)?;
            let end = i64::try_from(high)
                .map_err(|_| Error::ArithmeticOverflow)?
                .checked_add(1)
                .ok_or(Error::ArithmeticOverflow)?;
            endpoints.extend([start, end]);
            intervals.push(Some(start..end));
        } else {
            intervals.push(None);
        }
    }
    let endpoints = endpoints.into_iter().collect::<Vec<_>>();
    for pair in endpoints.windows(2) {
        let active = intervals
            .iter()
            .enumerate()
            .filter_map(|(s, interval)| {
                interval
                    .as_ref()
                    .is_some_and(|range| range.contains(&pair[0]))
                    .then_some(s)
            })
            .collect::<Vec<_>>();
        if active.is_empty() {
            continue;
        }
        let mut piece = plan.clone();
        piece.statements = active.iter().map(|&s| plan.statements[s].clone()).collect();
        let sources = active
            .iter()
            .map(|&s| bodies[s].clone())
            .collect::<Vec<_>>();
        let start = immediate(builder, pair[0]);
        let end = immediate(builder, pair[1] - 1);
        interior_loop(builder, &piece, &sources, prefix, start, end, options)?;
    }
    Ok(true)
}

/// Intersect statement domains on the innermost non-scalar coordinate. Their
/// shared interval needs no per-instance guards; only its two boundary pieces
/// use the general union scanner. This derives bounds from the scattering
/// polyhedra and also works with skewed, shifted and partially filled tiles.
fn interior<A: Clone + Eq + Hash>(
    builder: &mut SIRBuilder<A>,
    plan: &ScanPlan,
    bodies: &[Body<'_, A>],
    prefix: &mut Vec<Value>,
    lower: Value,
    upper: Value,
    options: &CodegenOptions,
) -> Result<()> {
    let axis = prefix.len();
    let zero = immediate(builder, 0);
    let one = immediate(builder, 1);
    let mut valid = one;
    let mut start = lower;
    let mut end = upper;
    for statement in &plan.statements {
        for row in &statement.domain {
            let mut numerator = Affine::new(row.coefficients[..axis].to_vec(), row.constant);
            for (j, &coefficient) in row.coefficients.iter().enumerate().skip(axis + 1) {
                numerator.constant = i64::try_from(
                    i128::from(numerator.constant)
                        + i128::from(coefficient)
                            * i128::from(statement.fixed_coordinates[j].unwrap()),
                )
                .map_err(|_| Error::ArithmeticOverflow)?;
            }
            let coefficient = row.coefficients[axis];
            if coefficient == 0 {
                let value = expression(builder, &numerator, prefix)?;
                let check = binary(builder, value, BinaryOp::GeS, zero)?;
                valid = select(builder, check, valid, zero);
            } else {
                if coefficient > 0 {
                    numerator.constant = numerator
                        .constant
                        .checked_neg()
                        .ok_or(Error::ArithmeticOverflow)?;
                    for c in &mut numerator.coefficients {
                        *c = c.checked_neg().ok_or(Error::ArithmeticOverflow)?;
                    }
                }
                let bound = Bound {
                    numerator,
                    denominator: coefficient.checked_abs().ok_or(Error::ArithmeticOverflow)?,
                };
                let value = self::bound(builder, &bound, prefix, coefficient > 0)?;
                if coefficient > 0 {
                    start = extremum(builder, &[start, value], false)?;
                } else {
                    end = extremum(builder, &[end, value], true)?;
                }
            }
        }
    }
    let nonempty = binary(builder, start, BinaryOp::LeS, end)?;
    valid = select(builder, nonempty, valid, zero);
    let after_upper = binary(builder, upper, BinaryOp::Add, one)?;
    // An empty intersection puts the whole union in the prefix piece.
    start = select(builder, valid, start, after_upper);
    end = select(builder, valid, end, upper);
    let before = binary(builder, start, BinaryOp::Sub, one)?;
    point_loop(builder, lower, before, |builder, value| {
        prefix.push(value);
        walk(builder, plan, bodies, prefix, options)?;
        prefix.pop();
        Ok(())
    })?;
    interior_loop(builder, plan, bodies, prefix, start, end, options)?;
    let after = binary(builder, end, BinaryOp::Add, one)?;
    point_loop(builder, after, upper, |builder, value| {
        prefix.push(value);
        walk(builder, plan, bodies, prefix, options)?;
        prefix.pop();
        Ok(())
    })
}

fn walk<A: Clone + Eq + Hash>(
    builder: &mut SIRBuilder<A>,
    plan: &ScanPlan,
    bodies: &[Body<'_, A>],
    prefix: &mut Vec<Value>,
    options: &CodegenOptions,
) -> Result<()> {
    if prefix.len() == plan.dimensions.len() {
        for (source, statement) in bodies.iter().zip(&plan.statements) {
            let zero = immediate(builder, 0);
            let mut condition = immediate(builder, 1);
            for guard in &statement.guards {
                let value = expression(builder, guard, prefix)?;
                let check = binary(builder, value, BinaryOp::GeS, zero)?;
                // Conditions have width one; use Mux to keep a consistent i64
                // accumulator instead of mixing bit widths in an And.
                condition = select(builder, check, condition, zero);
            }
            guarded(builder, condition, |builder| {
                body(builder, source, &statement.iterators, prefix)
            })?;
        }
        return Ok(());
    }
    let dimension = &plan.dimensions[prefix.len()];
    let lower = union_bound(builder, &dimension.lower, prefix, true)?;
    let upper = union_bound(builder, &dimension.upper, prefix, false)?;
    if let Some(values) = &dimension.fixed_values {
        for &value in values {
            let value = immediate(builder, value);
            let low = binary(builder, value, BinaryOp::GeS, lower)?;
            let high = binary(builder, value, BinaryOp::LeS, upper)?;
            let zero = immediate(builder, 0);
            let condition = select(builder, low, high, zero);
            prefix.push(value);
            guarded(builder, condition, |builder| {
                walk(builder, plan, bodies, prefix, options)
            })?;
            prefix.pop();
        }
        return Ok(());
    }
    if (plan.statements.len() > 1 || options.unroll > 1)
        && plan.statements.iter().all(|s| {
            s.fixed_coordinates[prefix.len() + 1..]
                .iter()
                .all(Option::is_some)
        })
    {
        if static_pieces(builder, plan, bodies, prefix, lower, upper, options)? {
            return Ok(());
        }
        return interior(builder, plan, bodies, prefix, lower, upper, options);
    }
    point_loop(builder, lower, upper, |builder, value| {
        prefix.push(value);
        walk(builder, plan, bodies, prefix, options)?;
        prefix.pop();
        Ok(())
    })
}

pub(super) fn lower<A: Clone + Eq + Hash>(
    bodies: &[StatementBody<A>],
    region: &Region,
    plan: &ScanPlan,
    options: &CodegenOptions,
) -> Result<ExecutionUnit<A>> {
    let bodies = bodies
        .iter()
        .zip(&region.statements)
        .map(|(source, statement)| Body {
            source,
            accesses: &statement.accesses,
        })
        .collect::<Vec<_>>();
    let mut builder = SIRBuilder::new();
    walk(&mut builder, plan, &bodies, &mut Vec::new(), options)?;
    builder.seal_block(SIRTerminator::Return);
    let (blocks, register_map, _) = builder.drain();
    if blocks.values().map(|b| b.instructions.len()).sum::<usize>() > options.max_instructions {
        return Err(Error::WorkLimit);
    }
    let unit = ExecutionUnit {
        blocks,
        register_map,
        entry_block_id: BlockId(0),
    };
    unit.verify_result()
        .map_err(|_| Error::Invalid("generated SIR failed SSA verification"))?;
    Ok(unit)
}
