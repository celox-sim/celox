use std::collections::BTreeSet;

use num_bigint::{BigInt, BigUint};
use num_traits::Zero;
use veryl_analyzer::ir::{
    ArrayLiteralItem, Comptime, Expression, Factor, Module, Statement, ValueVariant, VarId,
    VarIndex, VarSelect,
};
use veryl_analyzer::value::Value;
use veryl_parser::token_range::TokenRange;

use super::{
    BoundaryMap, NodeId, SLTForFoldGroupState, SLTNode, SLTNodeArena, SymbolicStore,
    combine_parts_with_default, eval_expression, eval_statement, get_width, merge_boundaries,
    range_store_error,
};
use crate::ir::{BitAccess, VarAtomBase};
use crate::logic_tree::range_store::RangeStore;
use crate::parser::bitaccess::is_static_access;
use crate::parser::loop_provenance::{LoopRecoveryCandidate, UnrolledLoopCandidate};
use crate::parser::resolve_total_width;
use crate::{HashMap, HashSet, ParserError};

pub(super) fn eval_statements(
    module: &Module,
    mut store: SymbolicStore<VarId>,
    mut boundaries: BoundaryMap<VarId>,
    statements: &[Statement],
    arena: &mut SLTNodeArena<VarId>,
    candidates: &[LoopRecoveryCandidate],
) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
    let mut index = 0usize;
    while index < statements.len() {
        if let Statement::If(if_stmt) = &statements[index]
            && if_stmt.false_side.is_empty()
        {
            let guarded_matches = matching_candidates(module, &if_stmt.true_side, candidates);
            if guarded_matches.len() == 1 {
                let candidate = guarded_matches[0];
                let ((guard, guard_sources), guard_boundaries) =
                    eval_expression(module, &store, &if_stmt.cond, arena, None)?;
                if let Some((next_store, recovered_boundaries)) = recover_group(
                    module,
                    &store,
                    &if_stmt.true_side,
                    candidate,
                    Some((guard, guard_sources)),
                    arena,
                )? {
                    store = next_store;
                    boundaries = merge_boundaries(
                        boundaries,
                        merge_boundaries(guard_boundaries, recovered_boundaries),
                    );
                    index += 1;
                    continue;
                }
            }
        }

        let direct_matches = candidates
            .iter()
            .filter_map(|candidate| {
                let run_len = candidate_run_len(&statements[index..], candidate.source.body_token);
                (run_len != 0
                    && exact_iteration_chunks(
                        module,
                        &statements[index..index + run_len],
                        candidate,
                    ))
                .then_some((candidate, run_len))
            })
            .collect::<Vec<_>>();
        if direct_matches.len() == 1 {
            let (candidate, run_len) = direct_matches[0];
            if let Some((next_store, recovered_boundaries)) = recover_group(
                module,
                &store,
                &statements[index..index + run_len],
                candidate,
                None,
                arena,
            )? {
                store = next_store;
                boundaries = merge_boundaries(boundaries, recovered_boundaries);
                index += run_len;
                continue;
            }
        }

        (store, boundaries) = eval_statement(module, store, boundaries, &statements[index], arena)?;
        index += 1;
    }

    Ok((store, boundaries))
}

fn matching_candidates<'a>(
    module: &Module,
    statements: &[Statement],
    candidates: &'a [LoopRecoveryCandidate],
) -> Vec<&'a LoopRecoveryCandidate> {
    candidates
        .iter()
        .filter(|candidate| exact_iteration_chunks(module, statements, candidate))
        .collect()
}

fn exact_iteration_chunks(
    module: &Module,
    statements: &[Statement],
    candidate: &LoopRecoveryCandidate,
) -> bool {
    let trip_count = candidate.unrolled.iterations.len();
    if trip_count < 2
        || statements.is_empty()
        || statements.len() % trip_count != 0
        || statements
            .iter()
            .any(|statement| !statement_in_range(statement, candidate.source.body_token))
    {
        return false;
    }

    let chunk_len = statements.len() / trip_count;
    let first = &statements[..chunk_len];
    for (iteration_index, chunk) in statements.chunks_exact(chunk_len).enumerate() {
        for (position, statement) in chunk.iter().enumerate() {
            if statement_token(statement) != statement_token(&first[position])
                || std::mem::discriminant(statement) != std::mem::discriminant(&first[position])
            {
                return false;
            }
            let mut variables = Vec::new();
            if !collect_statement_variables(statement, &mut variables) {
                return false;
            }
            for variable in variables {
                if let Some(owner) = iteration_owner(module, &candidate.unrolled, variable) {
                    if owner != iteration_index {
                        return false;
                    }
                }
            }
        }
    }
    true
}

fn candidate_run_len(statements: &[Statement], body: TokenRange) -> usize {
    statements
        .iter()
        .take_while(|statement| statement_in_range(statement, body))
        .count()
}

fn statement_in_range(statement: &Statement, outer: TokenRange) -> bool {
    statement_token(statement).is_some_and(|inner| token_range_contains(outer, inner))
}

fn token_range_contains(outer: TokenRange, inner: TokenRange) -> bool {
    outer.beg.source == inner.beg.source
        && outer.end.source == inner.end.source
        && outer.beg.pos <= inner.beg.pos
        && inner.end.pos.saturating_add(inner.end.length)
            <= outer.end.pos.saturating_add(outer.end.length)
}

fn statement_token(statement: &Statement) -> Option<TokenRange> {
    match statement {
        Statement::Assign(statement) => Some(statement.token),
        Statement::If(statement) => Some(statement.token),
        Statement::IfReset(statement) => Some(statement.token),
        Statement::Case(statement) => Some(statement.token),
        Statement::For(statement) => Some(statement.token),
        Statement::SystemFunctionCall(call) => Some(call.comptime.token),
        Statement::FunctionCall(call) => Some(call.comptime.token),
        Statement::TbMethodCall(_)
        | Statement::Break
        | Statement::Unsupported(_)
        | Statement::Null => None,
    }
}

fn iteration_owner(
    module: &Module,
    candidate: &UnrolledLoopCandidate,
    variable: VarId,
) -> Option<usize> {
    candidate
        .iterations
        .iter()
        .enumerate()
        .find_map(|(index, iteration)| {
            if variable == iteration.loop_var {
                return Some(index);
            }
            let path = &module.variables.get(&variable)?.path.0;
            (path.len() > iteration.hierarchy.len()
                && path.starts_with(iteration.hierarchy.as_slice()))
            .then_some(index)
        })
}

fn collect_statement_variables(statement: &Statement, out: &mut Vec<VarId>) -> bool {
    match statement {
        Statement::Assign(assign) => {
            if assign.dst.len() != 1 || !collect_expression_variables(&assign.expr, out) {
                return false;
            }
            let dst = &assign.dst[0];
            out.push(dst.id);
            is_static_access(&dst.index, &dst.select)
                && collect_index_variables(&dst.index, out)
                && collect_select_variables(&dst.select, out)
        }
        Statement::If(statement) => {
            collect_expression_variables(&statement.cond, out)
                && statement
                    .true_side
                    .iter()
                    .all(|statement| collect_statement_variables(statement, out))
                && statement
                    .false_side
                    .iter()
                    .all(|statement| collect_statement_variables(statement, out))
        }
        Statement::IfReset(_)
        | Statement::Case(_)
        | Statement::For(_)
        | Statement::SystemFunctionCall(_)
        | Statement::FunctionCall(_)
        | Statement::TbMethodCall(_)
        | Statement::Break
        | Statement::Unsupported(_)
        | Statement::Null => false,
    }
}

fn collect_index_variables(index: &VarIndex, out: &mut Vec<VarId>) -> bool {
    index
        .0
        .iter()
        .all(|expression| collect_expression_variables(expression, out))
}

fn collect_select_variables(select: &VarSelect, out: &mut Vec<VarId>) -> bool {
    select
        .0
        .iter()
        .all(|expression| collect_expression_variables(expression, out))
        && select
            .1
            .as_ref()
            .is_none_or(|(_, expression)| collect_expression_variables(expression, out))
}

fn collect_expression_variables(expression: &Expression, out: &mut Vec<VarId>) -> bool {
    match expression {
        Expression::Term(factor) => match factor.as_ref() {
            Factor::Variable(variable, index, select, _) => {
                out.push(*variable);
                collect_index_variables(index, out) && collect_select_variables(select, out)
            }
            Factor::Value(_) => true,
            Factor::SystemFunctionCall(_)
            | Factor::FunctionCall(_)
            | Factor::Anonymous(_)
            | Factor::Unknown(_) => false,
        },
        Expression::Unary(_, inner, _) => collect_expression_variables(inner, out),
        Expression::Binary(lhs, _, rhs, _) => {
            collect_expression_variables(lhs, out) && collect_expression_variables(rhs, out)
        }
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            collect_expression_variables(cond, out)
                && collect_expression_variables(then_expr, out)
                && collect_expression_variables(else_expr, out)
        }
        Expression::Concatenation(items, _) => items.iter().all(|(value, repeat)| {
            collect_expression_variables(value, out)
                && repeat
                    .as_ref()
                    .is_none_or(|repeat| collect_expression_variables(repeat, out))
        }),
        Expression::ArrayLiteral(items, _) => items.iter().all(|item| match item {
            ArrayLiteralItem::Value(value, repeat) => {
                collect_expression_variables(value, out)
                    && repeat
                        .as_ref()
                        .is_none_or(|repeat| collect_expression_variables(repeat, out))
            }
            ArrayLiteralItem::Defaul(value) => collect_expression_variables(value, out),
        }),
        Expression::StructConstructor(_, fields, _) => fields
            .iter()
            .all(|(_, value)| collect_expression_variables(value, out)),
    }
}

fn parameterize_folded_loop_values(
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

fn parameterize_statement(
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

fn parameterize_index(
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

fn parameterize_select(
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

fn parameterize_expression(
    module: &Module,
    template: &mut Expression,
    variants: &[&Expression],
    candidate: &UnrolledLoopCandidate,
) -> Option<bool> {
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

fn parameterize_factor(
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
        Factor::SystemFunctionCall(_)
        | Factor::FunctionCall(_)
        | Factor::Anonymous(_)
        | Factor::Unknown(_) => None,
    }
}

fn same_variable_position(
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

fn same_comptime_value(lhs: &Comptime, rhs: &Comptime) -> bool {
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

fn folded_value_matches_iteration(comptime: &Comptime, iteration: usize) -> bool {
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

struct ProvenGroup {
    arena: SLTNodeArena<VarId>,
    updates: Vec<NodeId>,
    update_sources: HashSet<VarAtomBase<VarId>>,
    boundaries: BoundaryMap<VarId>,
    targets: Vec<VarAtomBase<VarId>>,
    loop_var: VarId,
    loop_width: usize,
    loop_signed: bool,
    start: BigInt,
    step: BigInt,
    trip_count: usize,
}

fn recover_group(
    module: &Module,
    initial_store: &SymbolicStore<VarId>,
    statements: &[Statement],
    candidate: &LoopRecoveryCandidate,
    guard: Option<(NodeId, HashSet<VarAtomBase<VarId>>)>,
    arena: &mut SLTNodeArena<VarId>,
) -> Result<Option<(SymbolicStore<VarId>, BoundaryMap<VarId>)>, ParserError> {
    let Some(proof) = prove_group(module, initial_store, statements, candidate) else {
        return Ok(None);
    };
    if guard.is_none()
        && proof.targets.len() == 1
        && expanded_outputs_are_count_idioms(
            module,
            initial_store,
            statements,
            &proof.targets,
            arena,
        )
        .unwrap_or(false)
    {
        // The ordinary expansion has a stronger exact lowering (PopCount,
        // CLZ, or CTZ). Let the existing semantic matcher see that DAG rather
        // than replacing it with a counted loop.
        return Ok(None);
    }

    let (entry_guard, guard_sources) = if let Some(guard) = guard {
        guard
    } else {
        (
            arena.alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                1,
                false,
            ))?,
            HashSet::default(),
        )
    };

    let mut map_cache = HashMap::default();
    let updates = proof
        .updates
        .iter()
        .map(|&update| {
            proof
                .arena
                .get(update)
                .map_addr(update, &proof.arena, arena, &mut map_cache, &|id| *id)
        })
        .collect::<Result<Vec<_>, _>>()?;

    let state_ids = proof
        .targets
        .iter()
        .map(|target| target.id)
        .collect::<HashSet<_>>();
    let mut all_sources = proof.update_sources;
    all_sources.retain(|source| source.id != proof.loop_var && !state_ids.contains(&source.id));
    all_sources.extend(guard_sources);
    let mut initials = Vec::with_capacity(proof.targets.len());
    for target in &proof.targets {
        let range_store = initial_store.get(&target.id).ok_or_else(|| {
            ParserError::illegal_context(
                "recovered loop initial state",
                "state variable is absent from the symbolic store",
                None,
            )
        })?;
        let parts = range_store
            .get_parts(target.access)
            .map_err(|error| range_store_error("recovered loop initial state", error, None))?;
        if parts.iter().any(|(value, _)| value.is_none()) {
            return Ok(None);
        }
        let (initial, sources) =
            combine_parts_with_default(target.id, target.access.lsb, parts, arena)?;
        all_sources.extend(sources);
        initials.push(initial);
    }

    let states = proof
        .targets
        .iter()
        .copied()
        .zip(initials)
        .zip(updates)
        .map(|((target, initial), update)| SLTForFoldGroupState {
            target,
            initial,
            update,
        })
        .collect::<Vec<_>>();
    let group = arena.alloc(SLTNode::ForFoldGroup {
        loop_var: proof.loop_var,
        loop_width: proof.loop_width,
        loop_signed: proof.loop_signed,
        start: proof.start,
        step: proof.step,
        trip_count: proof.trip_count,
        entry_guard,
        states,
    })?;

    let total_width = get_width(group, arena);
    let mut next_msb = total_width;
    let mut result_store = initial_store.clone();
    for target in &proof.targets {
        let width = target.access.msb - target.access.lsb + 1;
        next_msb -= width;
        let projection = arena.alloc(SLTNode::Slice {
            expr: group,
            access: BitAccess::new(next_msb, next_msb + width - 1),
        })?;
        result_store
            .get_mut(&target.id)
            .ok_or_else(|| {
                ParserError::illegal_context(
                    "recovered loop result state",
                    "state variable is absent from the symbolic store",
                    None,
                )
            })?
            .update(target.access, Some((projection, all_sources.clone())))
            .map_err(|error| range_store_error("recovered loop result state", error, None))?;
    }

    Ok(Some((result_store, proof.boundaries)))
}

fn expanded_outputs_are_count_idioms(
    module: &Module,
    initial_store: &SymbolicStore<VarId>,
    statements: &[Statement],
    targets: &[VarAtomBase<VarId>],
    arena: &SLTNodeArena<VarId>,
) -> Option<bool> {
    // NodeIds stored in `initial_store` remain valid in an exact arena clone.
    // Evaluate only in this disposable copy: a non-count candidate must not
    // leave its unrolled DAG in the production arena.
    let mut scratch = arena.clone();
    let (store, _) = statements
        .iter()
        .try_fold(
            (initial_store.clone(), BoundaryMap::default()),
            |(store, boundaries), statement| {
                eval_statement(module, store, boundaries, statement, &mut scratch)
            },
        )
        .ok()?;

    targets
        .iter()
        .map(|target| {
            let parts = store.get(&target.id)?.get_parts(target.access).ok()?;
            let (output, _) =
                combine_parts_with_default(target.id, target.access.lsb, parts, &mut scratch)
                    .ok()?;
            Some(crate::logic_tree::matches_slt_count_idiom(output, &scratch))
        })
        .collect::<Option<Vec<_>>>()
        .map(|matches| matches.into_iter().any(|matched| matched))
}

fn prove_group(
    module: &Module,
    initial_store: &SymbolicStore<VarId>,
    statements: &[Statement],
    candidate: &LoopRecoveryCandidate,
) -> Option<ProvenGroup> {
    if !exact_iteration_chunks(module, statements, candidate) {
        return None;
    }
    let iterations = &candidate.unrolled.iterations;
    let first_iteration = iterations.first()?;
    let second_iteration = iterations.get(1)?;
    let step = BigInt::from(second_iteration.value) - BigInt::from(first_iteration.value);
    if step == BigInt::from(0u8)
        || iterations
            .windows(2)
            .any(|pair| BigInt::from(pair[1].value) - BigInt::from(pair[0].value) != step)
    {
        return None;
    }

    let loop_variable = module.variables.get(&first_iteration.loop_var)?;
    let loop_width = resolve_total_width(module, loop_variable).ok()?;
    if loop_width == 0 {
        return None;
    }

    let chunk_len = statements.len() / iterations.len();
    let chunks = statements.chunks_exact(chunk_len).collect::<Vec<_>>();
    let targets = persistent_full_width_targets(module, chunks[0], &candidate.unrolled)?;
    if targets.is_empty()
        || chunks.iter().skip(1).any(|chunk| {
            persistent_full_width_targets(module, chunk, &candidate.unrolled).as_ref()
                != Some(&targets)
        })
    {
        return None;
    }

    let target_ids = targets
        .iter()
        .map(|target| target.id)
        .collect::<HashSet<_>>();
    for target in &targets {
        let parts = initial_store
            .get(&target.id)?
            .get_parts(target.access)
            .ok()?;
        if parts.iter().any(|(value, _)| value.is_none()) {
            return None;
        }
        if parts.iter().any(|(value, _)| {
            value.as_ref().is_some_and(|(_, sources)| {
                sources.iter().any(|source| target_ids.contains(&source.id))
            })
        }) {
            return None;
        }
    }

    let mut first_template = chunks[0].to_vec();
    if !parameterize_folded_loop_values(module, &mut first_template, &chunks, &candidate.unrolled)?
    {
        return None;
    }
    let mut scratch = SLTNodeArena::new();
    for (iteration_index, (iteration, actual_chunk)) in
        iterations.iter().zip(chunks.iter()).enumerate()
    {
        let actual_outputs = eval_chunk_outputs(module, actual_chunk, &targets, &mut scratch)?;

        let mut concrete = first_template.clone();
        if !rewrite_statements(
            &mut concrete,
            first_iteration.loop_var,
            RewriteMode::Concrete(iteration.value),
        ) {
            return None;
        }
        let concrete_outputs = eval_chunk_outputs(module, &concrete, &targets, &mut scratch)?;
        if actual_outputs != concrete_outputs {
            return None;
        }

        let mut variables = Vec::new();
        for statement in actual_chunk.iter() {
            if !collect_statement_variables(statement, &mut variables)
                || variables.iter().any(|&variable| {
                    iteration_owner(module, &candidate.unrolled, variable)
                        .is_some_and(|owner| owner != iteration_index)
                })
            {
                return None;
            }
        }
    }

    let mut dynamic = first_template;
    if !rewrite_statements(&mut dynamic, first_iteration.loop_var, RewriteMode::Dynamic) {
        return None;
    }
    let (updates, update_sources, boundaries) =
        eval_chunk_outputs_with_facts(module, &dynamic, &targets, &mut scratch)?;

    let mut input_variables = HashSet::default();
    let mut visited = HashSet::default();
    for &update in &updates {
        if !collect_template_inputs(update, &scratch, &mut visited, &mut input_variables) {
            return None;
        }
    }
    if input_variables.iter().any(|&variable| {
        variable != first_iteration.loop_var
            && iteration_owner(module, &candidate.unrolled, variable).is_some()
    }) {
        return None;
    }

    for source in &update_sources {
        if source.id == first_iteration.loop_var || target_ids.contains(&source.id) {
            continue;
        }
        let parts = initial_store
            .get(&source.id)?
            .get_parts(source.access)
            .ok()?;
        if parts.iter().any(|(value, _)| value.is_some()) {
            return None;
        }
    }

    Some(ProvenGroup {
        arena: scratch,
        updates,
        update_sources,
        boundaries,
        targets,
        loop_var: first_iteration.loop_var,
        loop_width,
        loop_signed: loop_variable.r#type.signed,
        start: BigInt::from(first_iteration.value),
        step,
        trip_count: iterations.len(),
    })
}

fn persistent_full_width_targets(
    module: &Module,
    statements: &[Statement],
    candidate: &UnrolledLoopCandidate,
) -> Option<Vec<VarAtomBase<VarId>>> {
    let mut destinations = Vec::new();
    collect_destinations(statements, &mut destinations)?;
    let mut targets = BTreeSet::new();
    for destination in destinations {
        if iteration_owner(module, candidate, destination.id).is_some() {
            continue;
        }
        let width = resolve_total_width(module, module.variables.get(&destination.id)?).ok()?;
        if width == 0 || !is_static_access(&destination.index, &destination.select) {
            return None;
        }
        let access = crate::parser::bitaccess::eval_var_select(
            module,
            destination.id,
            &destination.index,
            &destination.select,
        )
        .ok()?;
        if access != BitAccess::new(0, width - 1) {
            return None;
        }
        targets.insert((destination.id, access.lsb, access.msb));
    }
    Some(
        targets
            .into_iter()
            .map(|(id, lsb, msb)| VarAtomBase::new(id, lsb, msb))
            .collect(),
    )
}

fn collect_destinations<'a>(
    statements: &'a [Statement],
    out: &mut Vec<&'a veryl_analyzer::ir::AssignDestination>,
) -> Option<()> {
    for statement in statements {
        match statement {
            Statement::Assign(assign) if assign.dst.len() == 1 => out.push(&assign.dst[0]),
            Statement::If(statement) => {
                collect_destinations(&statement.true_side, out)?;
                collect_destinations(&statement.false_side, out)?;
            }
            _ => return None,
        }
    }
    Some(())
}

fn marker_store(module: &Module) -> Option<SymbolicStore<VarId>> {
    let mut store = SymbolicStore::default();
    for (id, variable) in &module.variables {
        let width = resolve_total_width(module, variable).ok()?;
        store.insert(*id, RangeStore::new(None, width));
    }
    Some(store)
}

fn eval_chunk_outputs(
    module: &Module,
    statements: &[Statement],
    targets: &[VarAtomBase<VarId>],
    arena: &mut SLTNodeArena<VarId>,
) -> Option<Vec<NodeId>> {
    eval_chunk_outputs_with_facts(module, statements, targets, arena).map(|x| x.0)
}

fn eval_chunk_outputs_with_facts(
    module: &Module,
    statements: &[Statement],
    targets: &[VarAtomBase<VarId>],
    arena: &mut SLTNodeArena<VarId>,
) -> Option<(Vec<NodeId>, HashSet<VarAtomBase<VarId>>, BoundaryMap<VarId>)> {
    let store = marker_store(module)?;
    let (store, boundaries) = statements
        .iter()
        .try_fold(
            (store, BoundaryMap::default()),
            |(store, boundaries), statement| {
                eval_statement(module, store, boundaries, statement, arena)
            },
        )
        .ok()?;
    let mut outputs = Vec::with_capacity(targets.len());
    let mut sources = HashSet::default();
    for target in targets {
        let parts = store.get(&target.id)?.get_parts(target.access).ok()?;
        let (output, output_sources) =
            combine_parts_with_default(target.id, target.access.lsb, parts, arena).ok()?;
        outputs.push(output);
        sources.extend(output_sources);
    }
    Some((outputs, sources, boundaries))
}

enum RewriteMode {
    Concrete(usize),
    Dynamic,
}

fn rewrite_statements(statements: &mut [Statement], loop_var: VarId, mode: RewriteMode) -> bool {
    statements
        .iter_mut()
        .all(|statement| rewrite_statement(statement, loop_var, &mode))
}

fn rewrite_statement(statement: &mut Statement, loop_var: VarId, mode: &RewriteMode) -> bool {
    match statement {
        Statement::Assign(assign) if assign.dst.len() == 1 => {
            let destination = &mut assign.dst[0];
            let destination_depends = rewrite_index(&mut destination.index, loop_var, mode)
                | rewrite_select(&mut destination.select, loop_var, mode);
            !destination_depends && rewrite_expression(&mut assign.expr, loop_var, mode).is_some()
        }
        Statement::If(statement) => {
            rewrite_expression(&mut statement.cond, loop_var, mode).is_some()
                && statement
                    .true_side
                    .iter_mut()
                    .all(|statement| rewrite_statement(statement, loop_var, mode))
                && statement
                    .false_side
                    .iter_mut()
                    .all(|statement| rewrite_statement(statement, loop_var, mode))
        }
        _ => false,
    }
}

fn rewrite_index(index: &mut VarIndex, loop_var: VarId, mode: &RewriteMode) -> bool {
    index
        .0
        .iter_mut()
        .filter_map(|expression| rewrite_expression(expression, loop_var, mode))
        .fold(false, |depends, expression_depends| {
            depends | expression_depends
        })
}

fn rewrite_select(select: &mut VarSelect, loop_var: VarId, mode: &RewriteMode) -> bool {
    let mut depends = select
        .0
        .iter_mut()
        .filter_map(|expression| rewrite_expression(expression, loop_var, mode))
        .fold(false, |depends, expression_depends| {
            depends | expression_depends
        });
    if let Some((_, expression)) = &mut select.1
        && let Some(expression_depends) = rewrite_expression(expression, loop_var, mode)
    {
        depends |= expression_depends;
    }
    depends
}

fn rewrite_expression(
    expression: &mut Expression,
    loop_var: VarId,
    mode: &RewriteMode,
) -> Option<bool> {
    let depends = match expression {
        Expression::Term(factor) => match factor.as_mut() {
            Factor::Variable(variable, index, select, comptime) => {
                let index_depends = rewrite_index(index, loop_var, mode);
                let select_depends = rewrite_select(select, loop_var, mode);
                let self_depends = *variable == loop_var;
                if self_depends {
                    rewrite_loop_leaf(comptime, mode);
                } else if index_depends || select_depends {
                    invalidate_dependent_comptime(comptime, mode);
                }
                self_depends || index_depends || select_depends
            }
            Factor::Value(_) => false,
            Factor::SystemFunctionCall(_)
            | Factor::FunctionCall(_)
            | Factor::Anonymous(_)
            | Factor::Unknown(_) => return None,
        },
        Expression::Unary(_, inner, _) => rewrite_expression(inner, loop_var, mode)?,
        Expression::Binary(lhs, _, rhs, _) => {
            rewrite_expression(lhs, loop_var, mode)? | rewrite_expression(rhs, loop_var, mode)?
        }
        Expression::Ternary(cond, then_expr, else_expr, _) => {
            rewrite_expression(cond, loop_var, mode)?
                | rewrite_expression(then_expr, loop_var, mode)?
                | rewrite_expression(else_expr, loop_var, mode)?
        }
        Expression::Concatenation(items, _) => {
            let mut depends = false;
            for (value, repeat) in items {
                depends |= rewrite_expression(value, loop_var, mode)?;
                if let Some(repeat) = repeat {
                    depends |= rewrite_expression(repeat, loop_var, mode)?;
                }
            }
            depends
        }
        Expression::ArrayLiteral(items, _) => {
            let mut depends = false;
            for item in items {
                match item {
                    ArrayLiteralItem::Value(value, repeat) => {
                        depends |= rewrite_expression(value, loop_var, mode)?;
                        if let Some(repeat) = repeat {
                            depends |= rewrite_expression(repeat, loop_var, mode)?;
                        }
                    }
                    ArrayLiteralItem::Defaul(value) => {
                        depends |= rewrite_expression(value, loop_var, mode)?;
                    }
                }
            }
            depends
        }
        Expression::StructConstructor(_, fields, _) => {
            let mut depends = false;
            for (_, value) in fields {
                depends |= rewrite_expression(value, loop_var, mode)?;
            }
            depends
        }
    };
    if depends && !matches!(expression, Expression::Term(_)) {
        invalidate_dependent_comptime(expression.comptime_mut(), mode);
    }
    Some(depends)
}

fn rewrite_loop_leaf(comptime: &mut Comptime, mode: &RewriteMode) {
    match mode {
        RewriteMode::Concrete(value) => {
            let width = comptime.r#type.total_width().unwrap_or(0);
            comptime.value =
                ValueVariant::Numeric(Value::new(*value as u64, width, comptime.r#type.signed));
            comptime.is_const = true;
            comptime.expr_context.is_const = true;
            comptime.evaluated = true;
        }
        RewriteMode::Dynamic => {
            comptime.value = ValueVariant::Unknown;
            comptime.is_const = false;
            comptime.is_global = false;
            comptime.expr_context.is_const = false;
            comptime.expr_context.is_global = false;
            comptime.evaluated = false;
        }
    }
}

fn invalidate_dependent_comptime(comptime: &mut Comptime, mode: &RewriteMode) {
    comptime.value = ValueVariant::Unknown;
    comptime.evaluated = false;
    if matches!(mode, RewriteMode::Dynamic) {
        comptime.is_const = false;
        comptime.is_global = false;
        comptime.expr_context.is_const = false;
        comptime.expr_context.is_global = false;
    }
}

fn collect_template_inputs(
    root: NodeId,
    arena: &SLTNodeArena<VarId>,
    visited: &mut HashSet<NodeId>,
    inputs: &mut HashSet<VarId>,
) -> bool {
    if !visited.insert(root) {
        return true;
    }
    match arena.get(root) {
        SLTNode::Input {
            variable, index, ..
        } => {
            inputs.insert(*variable);
            index
                .iter()
                .all(|index| collect_template_inputs(index.node, arena, visited, inputs))
        }
        SLTNode::Constant(_, _, _, _) => true,
        SLTNode::Binary(lhs, _, rhs) => {
            collect_template_inputs(*lhs, arena, visited, inputs)
                && collect_template_inputs(*rhs, arena, visited, inputs)
        }
        SLTNode::Unary(_, inner) => collect_template_inputs(*inner, arena, visited, inputs),
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            collect_template_inputs(*cond, arena, visited, inputs)
                && collect_template_inputs(*then_expr, arena, visited, inputs)
                && collect_template_inputs(*else_expr, arena, visited, inputs)
        }
        SLTNode::Concat(parts) => parts
            .iter()
            .all(|(part, _)| collect_template_inputs(*part, arena, visited, inputs)),
        SLTNode::Slice { expr, .. } => collect_template_inputs(*expr, arena, visited, inputs),
        SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use veryl_analyzer::ir::{Component, Declaration, Ir, VarPath};
    use veryl_analyzer::{Analyzer, Context, attribute_table, symbol_table};
    use veryl_metadata::Metadata;
    use veryl_parser::Parser;
    use veryl_parser::resource_table;

    use super::*;
    use crate::logic_tree::comb::parse_comb_with_loop_recovery;
    use crate::parser::loop_provenance::{LoopProvenance, LoopSourceTable};

    fn analyze(code: &str) -> (Module, LoopProvenance) {
        symbol_table::clear();
        attribute_table::clear();

        let metadata = Metadata::create_default("prj").expect("default metadata must be valid");
        let analyzer = Analyzer::new(&metadata);
        let parsed = Parser::parse(code, &"").expect("test source must parse");
        let loop_sources = LoopSourceTable::collect([&parsed.veryl]);
        assert!(
            analyzer.analyze_pass1("prj", &parsed.veryl).is_empty(),
            "pass1 must succeed"
        );
        assert!(
            Analyzer::analyze_post_pass1().is_empty(),
            "post-pass1 must succeed"
        );

        let mut context = Context::default();
        let mut ir = Ir::default();
        assert!(
            analyzer
                .analyze_pass2("prj", &parsed.veryl, &mut context, Some(&mut ir))
                .is_empty(),
            "pass2 must succeed"
        );
        assert!(
            Analyzer::analyze_post_pass2(&ir).is_empty(),
            "post-pass2 must succeed"
        );
        let provenance = loop_sources.match_unrolled(&ir);
        let top = resource_table::insert_str("Top");
        let module = ir
            .components
            .iter()
            .find_map(|component| match component {
                Component::Module(module) if module.name == top => Some(module.clone()),
                _ => None,
            })
            .expect("Top module must exist");
        (module, provenance)
    }

    fn parse_with_candidates(
        module: &Module,
        candidates: &[LoopRecoveryCandidate],
    ) -> (Vec<super::super::LogicPath<VarId>>, SLTNodeArena<VarId>) {
        let declaration = module
            .declarations
            .iter()
            .find_map(|declaration| match declaration {
                Declaration::Comb(comb) => Some(comb),
                _ => None,
            })
            .expect("always_comb must exist");
        let mut arena = SLTNodeArena::new();
        let (paths, _, _, _, _) =
            parse_comb_with_loop_recovery(module, declaration, &mut arena, candidates)
                .expect("comb lowering must succeed");
        (paths, arena)
    }

    fn variable(module: &Module, name: &str) -> VarId {
        let name = resource_table::insert_str(name);
        module
            .variables
            .values()
            .find(|variable| variable.path == VarPath(vec![name]))
            .map(|variable| variable.id)
            .expect("named variable must exist")
    }

    fn group_count(arena: &SLTNodeArena<VarId>) -> usize {
        arena
            .iter()
            .filter(|node| matches!(node, SLTNode::ForFoldGroup { .. }))
            .count()
    }

    const MULTI_STATE_LOOP: &str = r#"
        module Top (
            en   : input  logic,
            bits : input  logic<4>,
            value: output logic<3>,
        ) {
            var valid: logic;
            var age  : logic<3>;
            var data : logic<3>;
            always_comb {
                valid = 1'b0;
                age   = 3'd0;
                data  = 3'd0;
                if en {
                    for i in 0..4 {
                        let ai: logic<3> = (i as 3);
                        if bits[i] && (!valid || ai >: age) {
                            valid = 1'b1;
                            age   = ai;
                            data  = ai;
                        }
                    }
                }
                value = data;
            }
        }
    "#;

    #[test]
    fn recovers_guarded_multi_state_loop_as_one_shared_group() {
        let (module, provenance) = analyze(MULTI_STATE_LOOP);
        let candidates = provenance.candidates_for_module(&module);
        assert_eq!(candidates.len(), 1);
        let (paths, arena) = parse_with_candidates(&module, &candidates);

        assert_eq!(group_count(&arena), 1);
        let (group_id, loop_var, states, entry_guard, trip_count) = arena
            .iter()
            .enumerate()
            .find_map(|(index, node)| match node {
                SLTNode::ForFoldGroup {
                    loop_var,
                    states,
                    entry_guard,
                    trip_count,
                    ..
                } => Some((NodeId(index), *loop_var, states, *entry_guard, *trip_count)),
                _ => None,
            })
            .expect("group must exist");
        assert_eq!(trip_count, 4);
        assert_eq!(states.len(), 3);
        assert!(!matches!(arena.get(entry_guard), SLTNode::Constant(..)));
        assert!(states.iter().any(|state| {
            let mut visited = HashSet::default();
            update_has_dynamic_loop_index(state.update, loop_var, &arena, &mut visited)
        }));

        for name in ["valid", "age", "data"] {
            let id = variable(&module, name);
            let path = paths
                .iter()
                .find(|path| path.target.var().is_some_and(|target| target.id == id))
                .expect("state path must exist");
            assert!(matches!(
                arena.get(path.expr),
                SLTNode::Slice { expr, .. } if *expr == group_id
            ));
        }
    }

    #[test]
    fn rejects_non_affine_provenance_values() {
        let (module, provenance) = analyze(MULTI_STATE_LOOP);
        let mut candidates = provenance.candidates_for_module(&module);
        candidates[0].unrolled.iterations[2].value = 9;
        let (_, arena) = parse_with_candidates(&module, &candidates);
        assert_eq!(group_count(&arena), 0);
    }

    #[test]
    fn rejects_ambiguous_duplicate_candidate() {
        let (module, provenance) = analyze(MULTI_STATE_LOOP);
        let mut candidates = provenance.candidates_for_module(&module);
        candidates.push(candidates[0].clone());
        let (_, arena) = parse_with_candidates(&module, &candidates);
        assert_eq!(group_count(&arena), 0);
    }

    #[test]
    fn rejects_state_initial_value_that_depends_on_the_same_state() {
        let (module, provenance) = analyze(MULTI_STATE_LOOP);
        let candidates = provenance.candidates_for_module(&module);
        let declaration = module
            .declarations
            .iter()
            .find_map(|declaration| match declaration {
                Declaration::Comb(comb) => Some(comb),
                _ => None,
            })
            .unwrap();
        let guarded_body = declaration
            .statements
            .iter()
            .find_map(|statement| match statement {
                Statement::If(statement) => Some(statement.true_side.as_slice()),
                _ => None,
            })
            .unwrap();

        let mut arena = SLTNodeArena::new();
        let mut initial_store = marker_store(&module).unwrap();
        for name in ["valid", "age", "data"] {
            let id = variable(&module, name);
            let width = resolve_total_width(&module, &module.variables[&id]).unwrap();
            let node = if name == "valid" {
                arena
                    .alloc(SLTNode::Input {
                        variable: id,
                        signed: false,
                        index: Vec::new(),
                        access: BitAccess::new(0, width - 1),
                    })
                    .unwrap()
            } else {
                arena
                    .alloc(SLTNode::Constant(
                        BigUint::from(0u8),
                        BigUint::from(0u8),
                        width,
                        false,
                    ))
                    .unwrap()
            };
            let mut sources = HashSet::default();
            if name == "valid" {
                sources.insert(VarAtomBase::new(id, 0, width - 1));
            }
            initial_store.insert(id, RangeStore::new(Some((node, sources)), width));
        }

        let recovered = recover_group(
            &module,
            &initial_store,
            guarded_body,
            &candidates[0],
            None,
            &mut arena,
        )
        .unwrap();
        assert!(recovered.is_none());
    }

    #[test]
    fn rejects_loop_dependent_destination() {
        let code = r#"
            module Top (
                bits : input  logic<4>,
                value: output logic<4>,
            ) {
                var state: logic<4>;
                always_comb {
                    state = 4'd0;
                    for i in 0..4 {
                        state[i] = bits[i];
                    }
                    value = state;
                }
            }
        "#;
        let (module, provenance) = analyze(code);
        let candidates = provenance.candidates_for_module(&module);
        let (_, arena) = parse_with_candidates(&module, &candidates);
        assert_eq!(group_count(&arena), 0);
    }

    fn update_has_dynamic_loop_index(
        root: NodeId,
        loop_var: VarId,
        arena: &SLTNodeArena<VarId>,
        visited: &mut HashSet<NodeId>,
    ) -> bool {
        if !visited.insert(root) {
            return false;
        }
        match arena.get(root) {
            SLTNode::Input { index, .. } => index.iter().any(|index| {
                let mut index_inputs = HashSet::default();
                let mut index_visited = HashSet::default();
                collect_template_inputs(index.node, arena, &mut index_visited, &mut index_inputs)
                    && index_inputs.contains(&loop_var)
            }),
            SLTNode::Binary(lhs, _, rhs) => {
                update_has_dynamic_loop_index(*lhs, loop_var, arena, visited)
                    || update_has_dynamic_loop_index(*rhs, loop_var, arena, visited)
            }
            SLTNode::Unary(_, inner) => {
                update_has_dynamic_loop_index(*inner, loop_var, arena, visited)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                update_has_dynamic_loop_index(*cond, loop_var, arena, visited)
                    || update_has_dynamic_loop_index(*then_expr, loop_var, arena, visited)
                    || update_has_dynamic_loop_index(*else_expr, loop_var, arena, visited)
            }
            SLTNode::Concat(parts) => parts
                .iter()
                .any(|(part, _)| update_has_dynamic_loop_index(*part, loop_var, arena, visited)),
            SLTNode::Slice { expr, .. } => {
                update_has_dynamic_loop_index(*expr, loop_var, arena, visited)
            }
            SLTNode::Constant(..) | SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => false,
        }
    }
}
