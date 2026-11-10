//! Parameterizing the statements of one unrolled iteration by the loop variable.

use super::*;

pub(super) fn parameterize_folded_loop_values(
    module: &Module,
    template: &mut [Statement],
    chunks: &[&[Statement]],
    candidate: &UnrolledLoopCandidate,
) -> Option<bool> {
    if chunks.iter().any(|chunk| chunk.len() != template.len()) {
        return None;
    }
    let mut depends = false;
    for (position, statement) in template.iter_mut().enumerate() {
        let variants = chunks
            .iter()
            .map(|chunk| &chunk[position])
            .collect::<Vec<_>>();
        depends |= parameterize_statement(module, statement, &variants, candidate)?;
    }
    Some(depends)
}

pub(super) fn parameterize_statement(
    module: &Module,
    template: &mut Statement,
    variants: &[&Statement],
    candidate: &UnrolledLoopCandidate,
) -> Option<bool> {
    match template {
        Statement::Assign(template_assign) if template_assign.dst.len() == 1 => {
            let variant_assigns = variants
                .iter()
                .map(|statement| match statement {
                    Statement::Assign(assign) if assign.dst.len() == 1 => Some(assign),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            let template_dst = &mut template_assign.dst[0];
            if variant_assigns
                .iter()
                .enumerate()
                .any(|(iteration, assign)| {
                    !same_variable_position(
                        module,
                        candidate,
                        template_dst.id,
                        assign.dst[0].id,
                        iteration,
                    )
                })
            {
                return None;
            }
            let variant_indices = variant_assigns
                .iter()
                .map(|assign| &assign.dst[0].index)
                .collect::<Vec<_>>();
            let variant_selects = variant_assigns
                .iter()
                .map(|assign| &assign.dst[0].select)
                .collect::<Vec<_>>();
            let variant_exprs = variant_assigns
                .iter()
                .map(|assign| &assign.expr)
                .collect::<Vec<_>>();
            let mut depends =
                parameterize_index(module, &mut template_dst.index, &variant_indices, candidate)?;
            depends |= parameterize_select(
                module,
                &mut template_dst.select,
                &variant_selects,
                candidate,
            )?;
            depends |= parameterize_expression(
                module,
                &mut template_assign.expr,
                &variant_exprs,
                candidate,
            )?;
            Some(depends)
        }
        Statement::If(template_if) => {
            let variant_ifs = variants
                .iter()
                .map(|statement| match statement {
                    Statement::If(statement) => Some(statement),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            if variant_ifs.iter().any(|statement| {
                statement.true_side.len() != template_if.true_side.len()
                    || statement.false_side.len() != template_if.false_side.len()
            }) {
                return None;
            }
            let variant_conditions = variant_ifs
                .iter()
                .map(|statement| &statement.cond)
                .collect::<Vec<_>>();
            let mut depends = parameterize_expression(
                module,
                &mut template_if.cond,
                &variant_conditions,
                candidate,
            )?;
            for position in 0..template_if.true_side.len() {
                let nested = variant_ifs
                    .iter()
                    .map(|statement| &statement.true_side[position])
                    .collect::<Vec<_>>();
                depends |= parameterize_statement(
                    module,
                    &mut template_if.true_side[position],
                    &nested,
                    candidate,
                )?;
            }
            for position in 0..template_if.false_side.len() {
                let nested = variant_ifs
                    .iter()
                    .map(|statement| &statement.false_side[position])
                    .collect::<Vec<_>>();
                depends |= parameterize_statement(
                    module,
                    &mut template_if.false_side[position],
                    &nested,
                    candidate,
                )?;
            }
            Some(depends)
        }
        _ => None,
    }
}

pub(super) fn parameterize_index(
    module: &Module,
    template: &mut VarIndex,
    variants: &[&VarIndex],
    candidate: &UnrolledLoopCandidate,
) -> Option<bool> {
    if variants
        .iter()
        .any(|variant| variant.0.len() != template.0.len())
    {
        return None;
    }
    let mut depends = false;
    for position in 0..template.0.len() {
        let expressions = variants
            .iter()
            .map(|variant| &variant.0[position])
            .collect::<Vec<_>>();
        depends |=
            parameterize_expression(module, &mut template.0[position], &expressions, candidate)?;
    }
    Some(depends)
}

pub(super) fn parameterize_select(
    module: &Module,
    template: &mut VarSelect,
    variants: &[&VarSelect],
    candidate: &UnrolledLoopCandidate,
) -> Option<bool> {
    if variants
        .iter()
        .any(|variant| variant.0.len() != template.0.len())
    {
        return None;
    }
    let mut depends = false;
    for position in 0..template.0.len() {
        let expressions = variants
            .iter()
            .map(|variant| &variant.0[position])
            .collect::<Vec<_>>();
        depends |=
            parameterize_expression(module, &mut template.0[position], &expressions, candidate)?;
    }
    match &mut template.1 {
        None => {
            if variants.iter().any(|variant| variant.1.is_some()) {
                return None;
            }
        }
        Some((template_op, template_expression)) => {
            let expressions = variants
                .iter()
                .map(|variant| {
                    let (op, expression) = variant.1.as_ref()?;
                    (std::mem::discriminant(op) == std::mem::discriminant(template_op))
                        .then_some(expression)
                })
                .collect::<Option<Vec<_>>>()?;
            depends |=
                parameterize_expression(module, template_expression, &expressions, candidate)?;
        }
    }
    Some(depends)
}

pub(super) fn parameterize_expression(
    module: &Module,
    template: &mut Expression,
    variants: &[&Expression],
    candidate: &UnrolledLoopCandidate,
) -> Option<bool> {
    if let Some(affine) = parameterize_affine_folded_value(template, variants, candidate) {
        *template = affine;
        return Some(true);
    }
    if let Some((exact, _)) = parameterize_exact_folded_value(module, template, variants, candidate)
    {
        *template = exact;
        return Some(true);
    }
    if variants
        .iter()
        .any(|variant| !same_expression_shape(template, variant))
        && let Some(piecewise) = parameterize_single_folded_branch(module, variants, candidate)
    {
        *template = piecewise;
        return Some(true);
    }
    match template {
        Expression::Term(template_factor) => {
            let factors = variants
                .iter()
                .map(|expression| match expression {
                    Expression::Term(factor) => Some(factor.as_ref()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            parameterize_factor(module, template_factor.as_mut(), &factors, candidate)
        }
        Expression::Unary(template_op, template_inner, _) => {
            let inners = variants
                .iter()
                .map(|expression| match expression {
                    Expression::Unary(op, inner, _) if op == template_op => Some(inner.as_ref()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            parameterize_expression(module, template_inner, &inners, candidate)
        }
        Expression::Binary(template_lhs, template_op, template_rhs, _) => {
            let binaries = variants
                .iter()
                .map(|expression| match expression {
                    Expression::Binary(lhs, op, rhs, _) if op == template_op => {
                        Some((lhs.as_ref(), rhs.as_ref()))
                    }
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            let lhs = binaries.iter().map(|(lhs, _)| *lhs).collect::<Vec<_>>();
            let rhs = binaries.iter().map(|(_, rhs)| *rhs).collect::<Vec<_>>();
            Some(
                parameterize_expression(module, template_lhs, &lhs, candidate)?
                    | parameterize_expression(module, template_rhs, &rhs, candidate)?,
            )
        }
        Expression::Ternary(template_cond, template_then, template_else, _) => {
            let ternaries = variants
                .iter()
                .map(|expression| match expression {
                    Expression::Ternary(cond, then_expr, else_expr, _) => {
                        Some((cond.as_ref(), then_expr.as_ref(), else_expr.as_ref()))
                    }
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            let conditions = ternaries
                .iter()
                .map(|(condition, _, _)| *condition)
                .collect::<Vec<_>>();
            let then_exprs = ternaries
                .iter()
                .map(|(_, then_expr, _)| *then_expr)
                .collect::<Vec<_>>();
            let else_exprs = ternaries
                .iter()
                .map(|(_, _, else_expr)| *else_expr)
                .collect::<Vec<_>>();
            Some(
                parameterize_expression(module, template_cond, &conditions, candidate)?
                    | parameterize_expression(module, template_then, &then_exprs, candidate)?
                    | parameterize_expression(module, template_else, &else_exprs, candidate)?,
            )
        }
        Expression::Concatenation(template_items, _) => {
            let concatenations = variants
                .iter()
                .map(|expression| match expression {
                    Expression::Concatenation(items, _) if items.len() == template_items.len() => {
                        Some(items)
                    }
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            let mut depends = false;
            for position in 0..template_items.len() {
                let values = concatenations
                    .iter()
                    .map(|items| &items[position].0)
                    .collect::<Vec<_>>();
                depends |= parameterize_expression(
                    module,
                    &mut template_items[position].0,
                    &values,
                    candidate,
                )?;
                match &mut template_items[position].1 {
                    None => {
                        if concatenations
                            .iter()
                            .any(|items| items[position].1.is_some())
                        {
                            return None;
                        }
                    }
                    Some(template_repeat) => {
                        let repeats = concatenations
                            .iter()
                            .map(|items| items[position].1.as_ref())
                            .collect::<Option<Vec<_>>>()?;
                        depends |=
                            parameterize_expression(module, template_repeat, &repeats, candidate)?;
                    }
                }
            }
            Some(depends)
        }
        Expression::ArrayLiteral(_, _) | Expression::StructConstructor(_, _, _) => None,
    }
}

pub(super) fn same_expression_shape(lhs: &Expression, rhs: &Expression) -> bool {
    match (lhs, rhs) {
        (Expression::Term(lhs), Expression::Term(rhs)) => {
            std::mem::discriminant(lhs.as_ref()) == std::mem::discriminant(rhs.as_ref())
        }
        (Expression::Unary(lhs_op, lhs, _), Expression::Unary(rhs_op, rhs, _)) => {
            lhs_op == rhs_op && same_expression_shape(lhs, rhs)
        }
        (
            Expression::Binary(lhs_lhs, lhs_op, lhs_rhs, _),
            Expression::Binary(rhs_lhs, rhs_op, rhs_rhs, _),
        ) => {
            lhs_op == rhs_op
                && same_expression_shape(lhs_lhs, rhs_lhs)
                && same_expression_shape(lhs_rhs, rhs_rhs)
        }
        (
            Expression::Ternary(lhs_cond, lhs_then, lhs_else, _),
            Expression::Ternary(rhs_cond, rhs_then, rhs_else, _),
        ) => {
            same_expression_shape(lhs_cond, rhs_cond)
                && same_expression_shape(lhs_then, rhs_then)
                && same_expression_shape(lhs_else, rhs_else)
        }
        (Expression::Concatenation(lhs, _), Expression::Concatenation(rhs, _)) => {
            lhs.len() == rhs.len()
        }
        (Expression::ArrayLiteral(lhs, _), Expression::ArrayLiteral(rhs, _)) => {
            lhs.len() == rhs.len()
        }
        (
            Expression::StructConstructor(lhs_name, lhs, _),
            Expression::StructConstructor(rhs_name, rhs, _),
        ) => lhs_name == rhs_name && lhs.len() == rhs.len(),
        _ => false,
    }
}

/// Reconstruct a ternary that the analyzer folded independently in every
/// unrolled iteration. This covers source expressions such as
/// `if i == 0 ? init : values[i - 1]`, whose exceptional iteration no longer
/// has the same IR shape as the remaining iterations.
///
/// This is only a candidate template. `prove_group` still specializes it for
/// every iteration and compares it bit-for-bit with the expanded IR before a
/// loop is recovered.
pub(super) fn parameterize_single_folded_branch(
    module: &Module,
    variants: &[&Expression],
    candidate: &UnrolledLoopCandidate,
) -> Option<Expression> {
    if variants.len() < 3 || variants.len() != candidate.iterations.len() {
        return None;
    }

    for exceptional_index in 0..variants.len() {
        let retained_shape = variants
            .iter()
            .enumerate()
            .find_map(|(index, expression)| (index != exceptional_index).then_some(*expression))?;
        if variants.iter().enumerate().any(|(index, expression)| {
            index != exceptional_index && !same_expression_shape(retained_shape, expression)
        }) {
            continue;
        }
        let exceptional = variants[exceptional_index];
        let mut exceptional_variables = Vec::new();
        if !collect_expression_variables(exceptional, &mut exceptional_variables)
            || exceptional_variables
                .iter()
                .any(|&variable| iteration_owner(module, candidate, variable).is_some())
        {
            continue;
        }

        let retained_indices = (0..variants.len())
            .filter(|&index| index != exceptional_index)
            .collect::<Vec<_>>();
        let retained_variants = retained_indices
            .iter()
            .map(|&index| variants[index])
            .collect::<Vec<_>>();
        let mut reduced_candidate = candidate.clone();
        reduced_candidate.iterations = retained_indices
            .iter()
            .map(|&index| candidate.iterations[index].clone())
            .collect();

        let reduced_loop_var = reduced_candidate.iterations[0].loop_var;
        let canonical_loop_var = candidate.iterations[0].loop_var;
        let mut common = retained_variants[0].clone();
        if parameterize_expression(module, &mut common, &retained_variants, &reduced_candidate)
            .is_none()
        {
            continue;
        }
        remap_expression_variable(&mut common, reduced_loop_var, canonical_loop_var);

        let loop_variable = module.variables.get(&canonical_loop_var)?;
        let loop_width = resolve_total_width(module, loop_variable).ok()?;
        if loop_width == 0 {
            return None;
        }
        let token = exceptional.comptime().token;
        let mut loop_comptime = Comptime::from_type(
            loop_variable.r#type.clone(),
            exceptional.comptime().clock_domain,
            token,
        );
        loop_comptime.expr_context.width = loop_width;
        loop_comptime.expr_context.signed = loop_variable.r#type.signed;
        let loop_value = Expression::Term(Box::new(Factor::Variable(
            canonical_loop_var,
            VarIndex::default(),
            VarSelect::default(),
            loop_comptime,
        )));
        let iteration_value = Expression::Term(Box::new(Factor::create_value(
            Value::new_biguint(
                signed_to_bits(
                    BigInt::from(candidate.iterations[exceptional_index].value),
                    loop_width,
                )?,
                loop_width,
                loop_variable.r#type.signed,
            ),
            token,
        )));
        let mut condition_comptime = Comptime::create_value(Value::new(0, 1, false), token);
        invalidate_dependent_comptime(&mut condition_comptime, &RewriteMode::Dynamic);
        let condition = Expression::Binary(
            Box::new(loop_value),
            Op::Eq,
            Box::new(iteration_value),
            Box::new(condition_comptime),
        );
        guard_loop_dependent_accesses(&mut common, canonical_loop_var, &condition);
        let mut result_comptime = exceptional.comptime().clone();
        invalidate_dependent_comptime(&mut result_comptime, &RewriteMode::Dynamic);
        return Some(Expression::Ternary(
            Box::new(condition),
            Box::new(exceptional.clone()),
            Box::new(common),
            Box::new(result_comptime),
        ));
    }
    None
}

pub(super) fn guard_loop_dependent_accesses(
    expression: &mut Expression,
    loop_var: VarId,
    exceptional_condition: &Expression,
) {
    fn guard_access(
        expression: &mut Expression,
        loop_var: VarId,
        exceptional_condition: &Expression,
    ) {
        guard_loop_dependent_accesses(expression, loop_var, exceptional_condition);
        if !expression_contains_variable(expression, loop_var) {
            return;
        }
        let comptime = expression.comptime().clone();
        let width = comptime.r#type.total_width().unwrap_or(32).max(1);
        let zero = Expression::Term(Box::new(Factor::create_value(
            Value::new(0, width, comptime.r#type.signed),
            comptime.token,
        )));
        let dynamic = std::mem::replace(expression, zero.clone());
        let mut result_comptime = comptime;
        invalidate_dependent_comptime(&mut result_comptime, &RewriteMode::Dynamic);
        *expression = Expression::Ternary(
            Box::new(exceptional_condition.clone()),
            Box::new(zero),
            Box::new(dynamic),
            Box::new(result_comptime),
        );
    }

    match expression {
        Expression::Term(factor) => match factor.as_mut() {
            Factor::Variable(_, index, select, _) => {
                for expression in &mut index.0 {
                    guard_access(expression, loop_var, exceptional_condition);
                }
                for expression in &mut select.0 {
                    guard_access(expression, loop_var, exceptional_condition);
                }
                if let Some((_, expression)) = &mut select.1 {
                    guard_access(expression, loop_var, exceptional_condition);
                }
            }
            Factor::FunctionCall(call) if call.outputs.is_empty() => {
                for input in call.inputs.values_mut() {
                    guard_loop_dependent_accesses(input, loop_var, exceptional_condition);
                }
            }
            _ => {}
        },
        Expression::Unary(_, inner, _) => {
            guard_loop_dependent_accesses(inner, loop_var, exceptional_condition);
        }
        Expression::Binary(lhs, _, rhs, _) => {
            guard_loop_dependent_accesses(lhs, loop_var, exceptional_condition);
            guard_loop_dependent_accesses(rhs, loop_var, exceptional_condition);
        }
        Expression::Ternary(condition, then_expr, else_expr, _) => {
            guard_loop_dependent_accesses(condition, loop_var, exceptional_condition);
            guard_loop_dependent_accesses(then_expr, loop_var, exceptional_condition);
            guard_loop_dependent_accesses(else_expr, loop_var, exceptional_condition);
        }
        Expression::Concatenation(items, _) => {
            for (value, repeat) in items {
                guard_loop_dependent_accesses(value, loop_var, exceptional_condition);
                if let Some(repeat) = repeat {
                    guard_loop_dependent_accesses(repeat, loop_var, exceptional_condition);
                }
            }
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(value, repeat) => {
                        guard_loop_dependent_accesses(value, loop_var, exceptional_condition);
                        if let Some(repeat) = repeat {
                            guard_loop_dependent_accesses(repeat, loop_var, exceptional_condition);
                        }
                    }
                    ArrayLiteralItem::Defaul(value) => {
                        guard_loop_dependent_accesses(value, loop_var, exceptional_condition);
                    }
                }
            }
        }
        Expression::StructConstructor(_, fields, _) => {
            for (_, value) in fields {
                guard_loop_dependent_accesses(value, loop_var, exceptional_condition);
            }
        }
    }
}

pub(super) fn expression_contains_variable(expression: &Expression, target: VarId) -> bool {
    let mut variables = Vec::new();
    collect_expression_variables(expression, &mut variables) && variables.contains(&target)
}

pub(super) fn remap_expression_variable(expression: &mut Expression, from: VarId, to: VarId) {
    match expression {
        Expression::Term(factor) => match factor.as_mut() {
            Factor::Variable(variable, index, select, _) => {
                if *variable == from {
                    *variable = to;
                }
                for expression in &mut index.0 {
                    remap_expression_variable(expression, from, to);
                }
                for expression in &mut select.0 {
                    remap_expression_variable(expression, from, to);
                }
                if let Some((_, expression)) = &mut select.1 {
                    remap_expression_variable(expression, from, to);
                }
            }
            Factor::FunctionCall(call) if call.outputs.is_empty() => {
                for input in call.inputs.values_mut() {
                    remap_expression_variable(input, from, to);
                }
            }
            _ => {}
        },
        Expression::Unary(_, inner, _) => remap_expression_variable(inner, from, to),
        Expression::Binary(lhs, _, rhs, _) => {
            remap_expression_variable(lhs, from, to);
            remap_expression_variable(rhs, from, to);
        }
        Expression::Ternary(condition, then_expr, else_expr, _) => {
            remap_expression_variable(condition, from, to);
            remap_expression_variable(then_expr, from, to);
            remap_expression_variable(else_expr, from, to);
        }
        Expression::Concatenation(items, _) => {
            for (value, repeat) in items {
                remap_expression_variable(value, from, to);
                if let Some(repeat) = repeat {
                    remap_expression_variable(repeat, from, to);
                }
            }
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(value, repeat) => {
                        remap_expression_variable(value, from, to);
                        if let Some(repeat) = repeat {
                            remap_expression_variable(repeat, from, to);
                        }
                    }
                    ArrayLiteralItem::Defaul(value) => {
                        remap_expression_variable(value, from, to);
                    }
                }
            }
        }
        Expression::StructConstructor(_, fields, _) => {
            for (_, value) in fields {
                remap_expression_variable(value, from, to);
            }
        }
    }
}

pub(super) fn parameterize_factor(
    module: &Module,
    template: &mut Factor,
    variants: &[&Factor],
    candidate: &UnrolledLoopCandidate,
) -> Option<bool> {
    match template {
        Factor::Variable(template_variable, template_index, template_select, _) => {
            let variables = variants
                .iter()
                .map(|factor| match factor {
                    Factor::Variable(variable, index, select, _) => {
                        Some((*variable, index, select))
                    }
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            if variables
                .iter()
                .enumerate()
                .any(|(iteration, (variable, _, _))| {
                    !same_variable_position(
                        module,
                        candidate,
                        *template_variable,
                        *variable,
                        iteration,
                    )
                })
            {
                return None;
            }
            let indices = variables
                .iter()
                .map(|(_, index, _)| *index)
                .collect::<Vec<_>>();
            let selects = variables
                .iter()
                .map(|(_, _, select)| *select)
                .collect::<Vec<_>>();
            let is_loop_var = *template_variable == candidate.iterations[0].loop_var
                && variables.iter().enumerate().all(|(iteration, variable)| {
                    variable.0 == candidate.iterations[iteration].loop_var
                });
            Some(
                is_loop_var
                    | parameterize_index(module, template_index, &indices, candidate)?
                    | parameterize_select(module, template_select, &selects, candidate)?,
            )
        }
        Factor::HierVariable(_) => None,
        Factor::Value(template_comptime) => {
            let values = variants
                .iter()
                .map(|factor| match factor {
                    Factor::Value(comptime) => Some(comptime),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            if values
                .iter()
                .skip(1)
                .all(|value| same_comptime_value(values[0], value))
            {
                return Some(false);
            }
            if !values
                .iter()
                .zip(&candidate.iterations)
                .all(|(comptime, iteration)| {
                    folded_value_matches_iteration(comptime, iteration.value)
                })
            {
                return None;
            }
            let comptime = template_comptime.clone();
            *template = Factor::Variable(
                candidate.iterations[0].loop_var,
                VarIndex::default(),
                VarSelect::default(),
                comptime,
            );
            Some(true)
        }
        Factor::FunctionCall(template_call) if template_call.outputs.is_empty() => {
            let calls = variants
                .iter()
                .map(|factor| match factor {
                    Factor::FunctionCall(call)
                        if call.outputs.is_empty()
                            && call.id == template_call.id
                            && call.index == template_call.index
                            && call.inputs.len() == template_call.inputs.len() =>
                    {
                        Some(call)
                    }
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?;
            let mut depends = false;
            for (path, template_input) in template_call.inputs.iter_mut() {
                let inputs = calls
                    .iter()
                    .map(|call| function_call_arg(&call.inputs, path))
                    .collect::<Option<Vec<_>>>()?;
                depends |= parameterize_expression(module, template_input, &inputs, candidate)?;
            }
            Some(depends)
        }
        Factor::SystemFunctionCall(_)
        | Factor::FunctionCall(_)
        | Factor::Anonymous(_)
        | Factor::Unknown(_) => None,
    }
}

pub(super) fn same_variable_position(
    module: &Module,
    candidate: &UnrolledLoopCandidate,
    template: VarId,
    variant: VarId,
    iteration: usize,
) -> bool {
    let template_owner = iteration_owner(module, candidate, template);
    let variant_owner = iteration_owner(module, candidate, variant);
    match (template_owner, variant_owner) {
        (None, None) => template == variant,
        (Some(0), Some(owner)) if owner == iteration => {
            let Some(template_variable) = module.variables.get(&template) else {
                return false;
            };
            let Some(variant_variable) = module.variables.get(&variant) else {
                return false;
            };
            let template_prefix = candidate.iterations[0].hierarchy.len();
            let variant_prefix = candidate.iterations[iteration].hierarchy.len();
            template_variable.path.0.get(template_prefix..)
                == variant_variable.path.0.get(variant_prefix..)
                && template_variable.r#type.to_string() == variant_variable.r#type.to_string()
        }
        _ => false,
    }
}

pub(super) fn same_comptime_value(lhs: &Comptime, rhs: &Comptime) -> bool {
    let lhs_value = lhs.get_value().ok();
    let rhs_value = rhs.get_value().ok();
    match (lhs_value, rhs_value) {
        (Some(lhs), Some(rhs)) => {
            lhs.width() == rhs.width()
                && lhs.signed() == rhs.signed()
                && lhs.payload().as_ref() == rhs.payload().as_ref()
                && lhs.mask_xz().as_ref() == rhs.mask_xz().as_ref()
        }
        (None, None) => {
            format!("{:?}", lhs.value) == format!("{:?}", rhs.value)
                && lhs.r#type.to_string() == rhs.r#type.to_string()
        }
        _ => false,
    }
}

pub(super) fn folded_value_matches_iteration(comptime: &Comptime, iteration: usize) -> bool {
    let Ok(value) = comptime.get_value() else {
        return false;
    };
    let width = value.width();
    if width == 0 || !value.mask_xz().as_ref().is_zero() {
        return false;
    }
    let mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
    value.payload().as_ref() == &(BigUint::from(iteration) & mask)
}
