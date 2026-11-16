//! Recovering the loop-variable form of a value the analyzer folded per iteration.

use super::*;

pub(super) fn parameterize_affine_folded_value(
    template: &Expression,
    variants: &[&Expression],
    candidate: &UnrolledLoopCandidate,
) -> Option<Expression> {
    if variants.len() < 2 || variants.len() != candidate.iterations.len() {
        return None;
    }
    let template_comptime = match template {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::Value(comptime) => comptime,
            _ => return None,
        },
        _ => return None,
    };
    let width = template_comptime.get_value().ok()?.width();
    let signed = template_comptime.get_value().ok()?.signed();
    let values = variants
        .iter()
        .map(|expression| match expression {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Value(comptime)
                    if comptime.get_value().ok()?.width() == width
                        && comptime.get_value().ok()?.signed() == signed =>
                {
                    comptime_known_integer(comptime)
                }
                _ => None,
            },
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    if values.windows(2).all(|pair| pair[0] == pair[1]) {
        return None;
    }

    let x0 = BigInt::from(candidate.iterations[0].value);
    let x1 = BigInt::from(candidate.iterations[1].value);
    let dx = &x1 - &x0;
    if dx.is_zero() {
        return None;
    }
    let dy = &values[1] - &values[0];
    if &dy % &dx != BigInt::zero() {
        return None;
    }
    let slope = dy / dx;
    let intercept = &values[0] - &slope * &x0;
    if values
        .iter()
        .zip(&candidate.iterations)
        .any(|(actual, iteration)| actual != &(&slope * BigInt::from(iteration.value) + &intercept))
    {
        return None;
    }

    let expression_comptime = template_comptime.clone();
    let loop_value = Expression::Term(Box::new(Factor::Variable(
        candidate.iterations[0].loop_var,
        VarIndex::default(),
        VarSelect::default(),
        expression_comptime.clone(),
    )));
    if slope == -BigInt::one() {
        let offset = Expression::Term(Box::new(Factor::create_value(
            Value::new_biguint(signed_to_bits(intercept, width)?, width, signed),
            template_comptime.token,
        )));
        return Some(Expression::Binary(
            Box::new(offset),
            Op::Sub,
            Box::new(loop_value),
            Box::new(expression_comptime),
        ));
    }
    let mut expression = loop_value;
    if slope != BigInt::one() {
        let factor = Expression::Term(Box::new(Factor::create_value(
            Value::new_biguint(signed_to_bits(slope, width)?, width, signed),
            template_comptime.token,
        )));
        expression = Expression::Binary(
            Box::new(expression),
            Op::Mul,
            Box::new(factor),
            Box::new(expression_comptime.clone()),
        );
    }
    if !intercept.is_zero() {
        let offset = Expression::Term(Box::new(Factor::create_value(
            Value::new_biguint(signed_to_bits(intercept, width)?, width, signed),
            template_comptime.token,
        )));
        expression = Expression::Binary(
            Box::new(expression),
            Op::Add,
            Box::new(offset),
            Box::new(expression_comptime),
        );
    }
    Some(expression)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ExactFoldedValueForm {
    Eq(BigInt),
    Ne(BigInt),
    Quotient {
        divisor: BigInt,
    },
    ModuloAffine {
        slope: BigInt,
        intercept: BigInt,
        modulus: BigInt,
    },
}

pub(super) struct FoldedValueSequence<'a> {
    pub(super) template_comptime: &'a Comptime,
    pub(super) output_width: usize,
    pub(super) output_signed: bool,
    pub(super) loop_width: usize,
    pub(super) loop_signed: bool,
    pub(super) domain: Vec<BigInt>,
    pub(super) values: Vec<BigInt>,
}

pub(super) fn parameterize_exact_folded_value(
    module: &Module,
    template: &Expression,
    variants: &[&Expression],
    candidate: &UnrolledLoopCandidate,
) -> Option<(Expression, ExactFoldedValueForm)> {
    let sequence = folded_value_sequence(module, template, variants, candidate)?;
    let forms = exact_folded_value_forms(&sequence);
    for form in forms {
        if exact_form_matches_sequence(&form, &sequence) {
            let expression =
                build_exact_folded_value_expression(module, candidate, &sequence, &form)?;
            return Some((expression, form));
        }
    }
    None
}

pub(super) fn folded_value_sequence<'a>(
    module: &Module,
    template: &'a Expression,
    variants: &[&Expression],
    candidate: &UnrolledLoopCandidate,
) -> Option<FoldedValueSequence<'a>> {
    if variants.len() < 2 || variants.len() != candidate.iterations.len() {
        return None;
    }
    let template_comptime = folded_value_comptime(template)?;
    let output = template_comptime.get_value().ok()?;
    let output_width = output.width();
    let output_signed = output.signed();
    if output_width == 0 {
        return None;
    }
    let values = variants
        .iter()
        .map(|expression| {
            let comptime = folded_value_comptime(expression)?;
            let value = comptime.get_value().ok()?;
            (value.width() == output_width && value.signed() == output_signed)
                .then(|| comptime_known_integer(comptime))?
        })
        .collect::<Option<Vec<_>>>()?;
    if values.windows(2).all(|pair| pair[0] == pair[1]) {
        return None;
    }

    let loop_variable = module.variables.get(&candidate.iterations[0].loop_var)?;
    let loop_width = resolve_total_width(module, loop_variable).ok()?;
    if loop_width == 0 {
        return None;
    }
    let loop_signed = loop_variable.r#type.signed;
    let domain = candidate
        .iterations
        .iter()
        .map(|iteration| {
            normalize_typed_integer(BigInt::from(iteration.value), loop_width, loop_signed)
        })
        .collect::<Option<Vec<_>>>()?;

    Some(FoldedValueSequence {
        template_comptime,
        output_width,
        output_signed,
        loop_width,
        loop_signed,
        domain,
        values,
    })
}

pub(super) fn folded_value_comptime(expression: &Expression) -> Option<&Comptime> {
    match expression {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::Value(comptime) => Some(comptime),
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn exact_folded_value_forms(
    sequence: &FoldedValueSequence<'_>,
) -> Vec<ExactFoldedValueForm> {
    let mut forms = Vec::new();

    if sequence.output_width == 1
        && sequence
            .values
            .iter()
            .all(|value| value == &BigInt::zero() || value == &BigInt::one())
    {
        let ones = sequence
            .values
            .iter()
            .enumerate()
            .filter_map(|(index, value)| (value == &BigInt::one()).then_some(index))
            .collect::<Vec<_>>();
        if let [index] = ones.as_slice() {
            forms.push(ExactFoldedValueForm::Eq(sequence.domain[*index].clone()));
        }
        let zeros = sequence
            .values
            .iter()
            .enumerate()
            .filter_map(|(index, value)| value.is_zero().then_some(index))
            .collect::<Vec<_>>();
        if let [index] = zeros.as_slice() {
            forms.push(ExactFoldedValueForm::Ne(sequence.domain[*index].clone()));
        }
    }

    if let Some(divisor) = quotient_divisor(sequence)
        && positive_power_of_two_log2(&divisor).is_some()
    {
        forms.push(ExactFoldedValueForm::Quotient { divisor });
    }
    forms.extend(modulo_affine_forms(sequence));
    forms
}

pub(super) fn quotient_divisor(sequence: &FoldedValueSequence<'_>) -> Option<BigInt> {
    if sequence
        .domain
        .iter()
        .chain(sequence.values.iter())
        .any(|value| value.is_negative())
    {
        return None;
    }

    // For y = floor(x / d):
    //   y == 0  => d > x
    //   y > 0   => x/(y+1) < d <= x/y.
    // Intersect those exact integer intervals; no search bound is needed.
    let mut lower = BigInt::from(2u8);
    let mut upper: Option<BigInt> = None;
    for (x, y) in sequence.domain.iter().zip(&sequence.values) {
        if y.is_zero() {
            let candidate = x + BigInt::one();
            if candidate > lower {
                lower = candidate;
            }
        } else {
            let candidate_lower = x / (y + BigInt::one()) + BigInt::one();
            if candidate_lower > lower {
                lower = candidate_lower;
            }
            let candidate_upper = x / y;
            if upper
                .as_ref()
                .is_none_or(|current| candidate_upper < *current)
            {
                upper = Some(candidate_upper);
            }
        }
    }
    upper.is_none_or(|upper| lower <= upper).then_some(lower)
}

pub(super) fn modulo_affine_forms(sequence: &FoldedValueSequence<'_>) -> Vec<ExactFoldedValueForm> {
    if sequence.domain.len() < 3 || sequence.values.iter().any(|value| value.is_negative()) {
        return Vec::new();
    }
    let step = &sequence.domain[1] - &sequence.domain[0];
    if step.is_zero()
        || sequence
            .domain
            .windows(2)
            .any(|pair| &pair[1] - &pair[0] != step)
    {
        return Vec::new();
    }
    let differences = sequence
        .values
        .windows(2)
        .map(|pair| &pair[1] - &pair[0])
        .collect::<Vec<_>>();

    // For y = (a*x + b) mod m and a constant x step, every adjacent
    // difference is either r or r-m, where r = a*step mod m.  Thus an exact
    // sequence has at most two distinct differences, and their distance is
    // the only possible modulus.  Derive that candidate in one pass instead
    // of comparing every pair of iterations.
    let first_difference = &differences[0];
    let mut second_difference = None;
    for difference in &differences[1..] {
        if difference == first_difference {
            continue;
        }
        match &second_difference {
            None => second_difference = Some(difference),
            Some(second) if *second == difference => {}
            Some(_) => return Vec::new(),
        }
    }
    let Some(second_difference) = second_difference else {
        return Vec::new();
    };
    let modulus = (first_difference - second_difference).abs();
    if modulus <= BigInt::one()
        || positive_power_of_two_log2(&modulus).is_none()
        || sequence.values.iter().any(|value| value >= &modulus)
    {
        return Vec::new();
    }

    let mut forms = Vec::new();
    let residue = positive_modulo(differences[0].clone(), &modulus);
    if differences
        .iter()
        .any(|difference| positive_modulo(difference.clone(), &modulus) != residue)
    {
        return Vec::new();
    }
    let mut slope_numerators = BTreeSet::new();
    slope_numerators.insert(residue.clone());
    slope_numerators.insert(&residue - &modulus);
    for numerator in slope_numerators {
        if &numerator % &step != BigInt::zero() {
            continue;
        }
        let slope = numerator / &step;
        let intercept =
            positive_modulo(&sequence.values[0] - &slope * &sequence.domain[0], &modulus);
        forms.push(ExactFoldedValueForm::ModuloAffine {
            slope,
            intercept,
            modulus: modulus.clone(),
        });
    }
    forms
}

pub(super) fn exact_form_matches_sequence(
    form: &ExactFoldedValueForm,
    sequence: &FoldedValueSequence<'_>,
) -> bool {
    sequence
        .domain
        .iter()
        .zip(&sequence.values)
        .all(|(input, expected)| {
            evaluate_exact_folded_value_form(form, input, sequence)
                .is_some_and(|actual| actual == *expected)
        })
}

pub(super) fn evaluate_exact_folded_value_form(
    form: &ExactFoldedValueForm,
    input: &BigInt,
    sequence: &FoldedValueSequence<'_>,
) -> Option<BigInt> {
    let input = normalize_typed_integer(input.clone(), sequence.loop_width, sequence.loop_signed)?;
    let result = match form {
        ExactFoldedValueForm::Eq(value) => BigInt::from(
            input
                == normalize_typed_integer(
                    value.clone(),
                    sequence.loop_width,
                    sequence.loop_signed,
                )?,
        ),
        ExactFoldedValueForm::Ne(value) => BigInt::from(
            input
                != normalize_typed_integer(
                    value.clone(),
                    sequence.loop_width,
                    sequence.loop_signed,
                )?,
        ),
        ExactFoldedValueForm::Quotient { divisor } => {
            let divisor = normalize_typed_integer(
                divisor.clone(),
                sequence.loop_width,
                sequence.loop_signed,
            )?;
            if divisor.is_zero() {
                return None;
            }
            input / divisor
        }
        ExactFoldedValueForm::ModuloAffine {
            slope,
            intercept,
            modulus,
        } => {
            let slope =
                normalize_typed_integer(slope.clone(), sequence.loop_width, sequence.loop_signed)?;
            let intercept = normalize_typed_integer(
                intercept.clone(),
                sequence.loop_width,
                sequence.loop_signed,
            )?;
            let modulus = normalize_typed_integer(
                modulus.clone(),
                sequence.loop_width,
                sequence.loop_signed,
            )?;
            if modulus.is_zero() {
                return None;
            }
            let product =
                normalize_typed_integer(input * slope, sequence.loop_width, sequence.loop_signed)?;
            let sum = normalize_typed_integer(
                product + intercept,
                sequence.loop_width,
                sequence.loop_signed,
            )?;
            sum % modulus
        }
    };
    normalize_typed_integer(result, sequence.output_width, sequence.output_signed)
}

pub(super) fn normalize_typed_integer(value: BigInt, width: usize, signed: bool) -> Option<BigInt> {
    let bits = signed_to_bits(value, width)?;
    if signed {
        Some(bits_to_signed(&bits, width))
    } else {
        Some(BigInt::from_biguint(Sign::Plus, bits))
    }
}

pub(super) fn positive_modulo(value: BigInt, modulus: &BigInt) -> BigInt {
    ((value % modulus) + modulus) % modulus
}

pub(super) fn build_exact_folded_value_expression(
    module: &Module,
    candidate: &UnrolledLoopCandidate,
    sequence: &FoldedValueSequence<'_>,
    form: &ExactFoldedValueForm,
) -> Option<Expression> {
    let loop_variable = module.variables.get(&candidate.iterations[0].loop_var)?;
    let mut loop_comptime = Comptime::from_type(
        loop_variable.r#type.clone(),
        sequence.template_comptime.clock_domain,
        sequence.template_comptime.token,
    );
    loop_comptime.expr_context.width = sequence.loop_width;
    loop_comptime.expr_context.signed = sequence.loop_signed;
    let loop_value = || {
        Expression::Term(Box::new(Factor::Variable(
            candidate.iterations[0].loop_var,
            VarIndex::default(),
            VarSelect::default(),
            loop_comptime.clone(),
        )))
    };
    let constant = |value: &BigInt| {
        let bits = signed_to_bits(value.clone(), sequence.loop_width)?;
        Some(Expression::Term(Box::new(Factor::create_value(
            Value::new_biguint(bits, sequence.loop_width, sequence.loop_signed),
            sequence.template_comptime.token,
        ))))
    };
    let binary = |lhs: Expression, op: Op, rhs: Expression, comptime: Comptime| {
        Expression::Binary(Box::new(lhs), op, Box::new(rhs), Box::new(comptime))
    };

    Some(match form {
        ExactFoldedValueForm::Eq(value) => binary(
            loop_value(),
            Op::Eq,
            constant(value)?,
            sequence.template_comptime.clone(),
        ),
        ExactFoldedValueForm::Ne(value) => binary(
            loop_value(),
            Op::Ne,
            constant(value)?,
            sequence.template_comptime.clone(),
        ),
        ExactFoldedValueForm::Quotient { divisor } => binary(
            loop_value(),
            Op::LogicShiftR,
            constant(&BigInt::from(positive_power_of_two_log2(divisor)?))?,
            sequence.template_comptime.clone(),
        ),
        ExactFoldedValueForm::ModuloAffine {
            slope,
            intercept,
            modulus,
        } => {
            let mut expression = loop_value();
            if slope != &BigInt::one() {
                expression = binary(expression, Op::Mul, constant(slope)?, loop_comptime.clone());
            }
            if !intercept.is_zero() {
                expression = binary(
                    expression,
                    Op::Add,
                    constant(intercept)?,
                    loop_comptime.clone(),
                );
            }
            positive_power_of_two_log2(modulus)?;
            binary(
                expression,
                Op::BitAnd,
                constant(&(modulus - BigInt::one()))?,
                sequence.template_comptime.clone(),
            )
        }
    })
}

pub(super) fn positive_power_of_two_log2(value: &BigInt) -> Option<usize> {
    let value = value.to_biguint()?;
    if value.is_zero() {
        return None;
    }
    let one = BigUint::one();
    ((&value & (&value - &one)).is_zero()).then(|| value.bits() as usize - 1)
}

pub(super) fn comptime_known_integer(comptime: &Comptime) -> Option<BigInt> {
    let value = comptime.get_value().ok()?;
    if !value.mask_xz().as_ref().is_zero() || value.width() == 0 {
        return None;
    }
    let payload = value.payload().as_ref().clone();
    if value.signed() && payload.bit((value.width() - 1).try_into().ok()?) {
        Some(BigInt::from_biguint(Sign::Plus, payload) - (BigInt::from(1u8) << value.width()))
    } else {
        Some(BigInt::from_biguint(Sign::Plus, payload))
    }
}
