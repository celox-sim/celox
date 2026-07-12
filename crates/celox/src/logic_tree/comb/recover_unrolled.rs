use std::collections::BTreeSet;

use num_bigint::{BigInt, BigUint, Sign};
use num_traits::{ToPrimitive, Zero};
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
use crate::ir::{BinaryOp, BitAccess, UnaryOp, VarAtomBase};
use crate::logic_tree::range_store::RangeStore;
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
            collect_index_variables(&dst.index, out) && collect_select_variables(&dst.select, out)
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
    let Some(proof) = prove_group(module, initial_store, statements, candidate, arena) else {
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
    production_arena: &SLTNodeArena<VarId>,
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
    let mut actual_outputs_by_iteration = Vec::with_capacity(iterations.len());
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
        actual_outputs_by_iteration.push(actual_outputs);

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

    // Canonical proof bits depend only on an immutable arena node and its bit
    // position.  The arena is append-only, so results remain valid while later
    // iterations add their specialized nodes.  Keep one cache for the whole
    // proof instead of rebuilding the same input/constant bits per iteration.
    let mut proof_bits = ProofBitCanonicalizer::default();
    for (iteration, actual_outputs) in iterations.iter().zip(&actual_outputs_by_iteration) {
        let mut dynamic_cache = HashMap::default();
        let specialized_updates = updates
            .iter()
            .map(|&update| {
                specialize_slt_node(
                    update,
                    Some((first_iteration.loop_var, iteration.value)),
                    None,
                    &mut scratch,
                    &mut dynamic_cache,
                )
            })
            .collect::<Option<Vec<_>>>()?;
        let mut actual_cache = HashMap::default();
        let canonical_actual = actual_outputs
            .iter()
            .map(|&actual| specialize_slt_node(actual, None, None, &mut scratch, &mut actual_cache))
            .collect::<Option<Vec<_>>>()?;
        if !proof_outputs_match(
            &specialized_updates,
            &canonical_actual,
            &mut scratch,
            &mut proof_bits,
        )? {
            return None;
        }
    }

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

    if !whole_fold_matches_expansion(
        module,
        initial_store,
        statements,
        production_arena,
        &scratch,
        &updates,
        &targets,
        first_iteration.loop_var,
        iterations,
    )? {
        return None;
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
        if width == 0 {
            return None;
        }
        let access = BitAccess::new(0, width - 1);
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

fn proof_variable_ids(
    statements: &[Statement],
    targets: &[VarAtomBase<VarId>],
) -> Option<HashSet<VarId>> {
    let mut variables = Vec::new();
    for statement in statements {
        collect_statement_variables(statement, &mut variables).then_some(())?;
    }
    let mut variables = variables.into_iter().collect::<HashSet<_>>();
    variables.extend(targets.iter().map(|target| target.id));
    Some(variables)
}

fn marker_store(module: &Module, variables: &HashSet<VarId>) -> Option<SymbolicStore<VarId>> {
    let mut store = SymbolicStore::default();
    for id in variables {
        let variable = module.variables.get(id)?;
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
    let variables = proof_variable_ids(statements, targets)?;
    let store = marker_store(module, &variables)?;
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

fn whole_fold_matches_expansion(
    module: &Module,
    initial_store: &SymbolicStore<VarId>,
    statements: &[Statement],
    production_arena: &SLTNodeArena<VarId>,
    dynamic_arena: &SLTNodeArena<VarId>,
    dynamic_updates: &[NodeId],
    targets: &[VarAtomBase<VarId>],
    loop_var: VarId,
    iterations: &[crate::parser::loop_provenance::UnrolledIteration],
) -> Option<bool> {
    let variables = proof_variable_ids(statements, targets)?;
    let mut proof_arena = SLTNodeArena::new();
    let mapped_initial = map_symbolic_store_roots(
        initial_store,
        &variables,
        production_arena,
        &mut proof_arena,
    )?;

    let (expanded_store, _) = statements
        .iter()
        .try_fold(
            (mapped_initial.clone(), BoundaryMap::default()),
            |(store, boundaries), statement| {
                eval_statement(module, store, boundaries, statement, &mut proof_arena)
            },
        )
        .ok()?;
    let expanded_outputs = read_target_outputs(&expanded_store, targets, &mut proof_arena)?;

    let mut update_map_cache = HashMap::default();
    let mapped_updates = dynamic_updates
        .iter()
        .map(|&update| {
            dynamic_arena
                .get(update)
                .map_addr(
                    update,
                    dynamic_arena,
                    &mut proof_arena,
                    &mut update_map_cache,
                    &|id| *id,
                )
                .ok()
        })
        .collect::<Option<Vec<_>>>()?;
    let initial_values = read_target_outputs(&mapped_initial, targets, &mut proof_arena)?;
    let mut state_values = targets
        .iter()
        .copied()
        .zip(initial_values)
        .map(|(target, value)| (target.id, (target.access, value)))
        .collect::<HashMap<_, _>>();

    for iteration in iterations {
        let mut cache = HashMap::default();
        let next_values = mapped_updates
            .iter()
            .map(|&update| {
                specialize_slt_node(
                    update,
                    Some((loop_var, iteration.value)),
                    Some(&state_values),
                    &mut proof_arena,
                    &mut cache,
                )
            })
            .collect::<Option<Vec<_>>>()?;
        state_values = targets
            .iter()
            .copied()
            .zip(next_values)
            .map(|(target, value)| (target.id, (target.access, value)))
            .collect();
    }

    let composed_outputs = targets
        .iter()
        .map(|target| state_values.get(&target.id).map(|(_, value)| *value))
        .collect::<Option<Vec<_>>>()?;
    let mut expanded_cache = HashMap::default();
    let canonical_expanded = expanded_outputs
        .iter()
        .map(|&output| {
            specialize_slt_node(output, None, None, &mut proof_arena, &mut expanded_cache)
        })
        .collect::<Option<Vec<_>>>()?;
    let mut composed_cache = HashMap::default();
    let canonical_composed = composed_outputs
        .iter()
        .map(|&output| {
            specialize_slt_node(output, None, None, &mut proof_arena, &mut composed_cache)
        })
        .collect::<Option<Vec<_>>>()?;
    let mut proof_bits = ProofBitCanonicalizer::default();
    proof_outputs_match(
        &canonical_expanded,
        &canonical_composed,
        &mut proof_arena,
        &mut proof_bits,
    )
}

fn map_symbolic_store_roots(
    store: &SymbolicStore<VarId>,
    variables: &HashSet<VarId>,
    source_arena: &SLTNodeArena<VarId>,
    target_arena: &mut SLTNodeArena<VarId>,
) -> Option<SymbolicStore<VarId>> {
    let mut mapped = SymbolicStore::default();
    let mut cache = HashMap::default();
    for id in variables {
        let mut range_store = store.get(id)?.clone();
        for (value, _, _) in range_store.ranges.values_mut() {
            let Some((node, _)) = value else {
                continue;
            };
            *node = source_arena
                .get(*node)
                .map_addr(*node, source_arena, target_arena, &mut cache, &|id| *id)
                .ok()?;
        }
        mapped.insert(*id, range_store);
    }
    Some(mapped)
}

fn read_target_outputs(
    store: &SymbolicStore<VarId>,
    targets: &[VarAtomBase<VarId>],
    arena: &mut SLTNodeArena<VarId>,
) -> Option<Vec<NodeId>> {
    targets
        .iter()
        .map(|target| {
            let parts = store.get(&target.id)?.get_parts(target.access).ok()?;
            combine_parts_with_default(target.id, target.access.lsb, parts, arena)
                .ok()
                .map(|(output, _)| output)
        })
        .collect()
}

#[derive(Clone)]
struct SpecializedConstant {
    value: BigUint,
    mask: BigUint,
    width: usize,
    signed: bool,
}

fn specialize_slt_node(
    node: NodeId,
    loop_value: Option<(VarId, usize)>,
    state_values: Option<&HashMap<VarId, (BitAccess, NodeId)>>,
    arena: &mut SLTNodeArena<VarId>,
    cache: &mut HashMap<NodeId, NodeId>,
) -> Option<NodeId> {
    if let Some(&specialized) = cache.get(&node) {
        return Some(specialized);
    }

    let original_width = get_width(node, arena);
    let specialized = match arena.get(node).clone() {
        SLTNode::Input {
            variable,
            signed,
            index,
            access,
        } => {
            if let Some(&(state_access, state_value)) =
                state_values.and_then(|values| values.get(&variable))
            {
                if !index.is_empty()
                    || access.lsb < state_access.lsb
                    || access.msb > state_access.msb
                {
                    return None;
                }
                if access == state_access {
                    state_value
                } else {
                    arena
                        .alloc(SLTNode::Slice {
                            expr: state_value,
                            access: BitAccess::new(
                                access.lsb - state_access.lsb,
                                access.msb - state_access.lsb,
                            ),
                        })
                        .ok()?
                }
            } else if loop_value.is_some_and(|(loop_var, _)| variable == loop_var) {
                if !index.is_empty() {
                    return None;
                }
                let (_, value) = loop_value?;
                let width = access.msb.checked_sub(access.lsb)?.checked_add(1)?;
                let value = (BigUint::from(value) >> access.lsb) & width_mask(width);
                arena
                    .alloc(SLTNode::Constant(value, BigUint::from(0u8), width, false))
                    .ok()?
            } else {
                let index = index
                    .into_iter()
                    .map(|mut entry| {
                        entry.node = specialize_slt_node(
                            entry.node,
                            loop_value,
                            state_values,
                            arena,
                            cache,
                        )?;
                        Some(entry)
                    })
                    .collect::<Option<Vec<_>>>()?;
                arena
                    .alloc(SLTNode::Input {
                        variable,
                        signed,
                        index,
                        access,
                    })
                    .ok()?
            }
        }
        // This arena is a proof-only canonical form. Signed value semantics are
        // already explicit in the opcode (DivS/RemS, signed comparisons, Sar)
        // and in coercion nodes, while analyzer-folded constants can retain a
        // signed tag that a semantically identical Slice-derived constant does
        // not. Erase that non-value tag so equality compares the generated bits.
        SLTNode::Constant(value, mask, width, _) => arena
            .alloc(SLTNode::Constant(value, mask, width, false))
            .ok()?,
        SLTNode::Binary(lhs, op, rhs) => {
            let lhs = specialize_slt_node(lhs, loop_value, state_values, arena, cache)?;
            let rhs = specialize_slt_node(rhs, loop_value, state_values, arena, cache)?;
            if let (Some(lhs_constant), Some(rhs_constant)) = (
                specialized_constant(lhs, arena),
                specialized_constant(rhs, arena),
            ) && let Some(result) = specialize_binary_constant(
                op,
                &lhs_constant,
                &rhs_constant,
                original_width,
                specialized_binary_result_signed(op, &lhs_constant, &rhs_constant),
            ) {
                alloc_specialized_constant(result, arena)?
            } else {
                arena.alloc(SLTNode::Binary(lhs, op, rhs)).ok()?
            }
        }
        SLTNode::Unary(op, inner) => {
            let inner = specialize_slt_node(inner, loop_value, state_values, arena, cache)?;
            if let Some(inner_constant) = specialized_constant(inner, arena)
                && let Some(result) = specialize_unary_constant(
                    op,
                    &inner_constant,
                    original_width,
                    specialized_unary_result_signed(op, &inner_constant),
                )
            {
                alloc_specialized_constant(result, arena)?
            } else {
                arena.alloc(SLTNode::Unary(op, inner)).ok()?
            }
        }
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } => {
            let cond = specialize_slt_node(cond, loop_value, state_values, arena, cache)?;
            let then_expr = specialize_slt_node(then_expr, loop_value, state_values, arena, cache)?;
            let else_expr = specialize_slt_node(else_expr, loop_value, state_values, arena, cache)?;
            if let Some(cond) = specialized_constant(cond, arena)
                && cond.mask.is_zero()
            {
                let selected = if cond.value.is_zero() {
                    else_expr
                } else {
                    then_expr
                };
                zero_extend_specialized(selected, original_width, arena)?
            } else {
                arena
                    .alloc(SLTNode::Mux {
                        cond,
                        then_expr,
                        else_expr,
                    })
                    .ok()?
            }
        }
        SLTNode::Concat(parts) => {
            let parts = parts
                .into_iter()
                .map(|(part, width)| {
                    Some((
                        specialize_slt_node(part, loop_value, state_values, arena, cache)?,
                        width,
                    ))
                })
                .collect::<Option<Vec<_>>>()?;
            if let Some(result) = specialize_concat_constant(&parts, arena) {
                alloc_specialized_constant(result, arena)?
            } else {
                arena.alloc(SLTNode::Concat(parts)).ok()?
            }
        }
        SLTNode::Slice { expr, access } => {
            let expr = specialize_slt_node(expr, loop_value, state_values, arena, cache)?;
            if let Some(constant) = specialized_constant(expr, arena) {
                let width = access.msb.checked_sub(access.lsb)?.checked_add(1)?;
                alloc_specialized_constant(
                    SpecializedConstant {
                        value: (&constant.value >> access.lsb) & width_mask(width),
                        mask: (&constant.mask >> access.lsb) & width_mask(width),
                        width,
                        signed: false,
                    },
                    arena,
                )?
            } else if let SLTNode::Input {
                variable,
                signed: _,
                index,
                access: input_access,
            } = arena.get(expr).clone()
            {
                let mut offset = 0usize;
                let mut all_indices_constant = true;
                for entry in &index {
                    let Some(value) = specialized_constant(entry.node, arena)
                        .filter(|value| value.mask.is_zero())
                        .and_then(|value| value.value.to_usize())
                    else {
                        all_indices_constant = false;
                        break;
                    };
                    offset = offset.checked_add(value.checked_mul(entry.stride)?)?;
                }
                if all_indices_constant {
                    let lsb = input_access
                        .lsb
                        .checked_add(offset)?
                        .checked_add(access.lsb)?;
                    let msb = input_access
                        .lsb
                        .checked_add(offset)?
                        .checked_add(access.msb)?;
                    if msb > input_access.msb {
                        return None;
                    }
                    arena
                        .alloc(SLTNode::Input {
                            variable,
                            signed: false,
                            index: Vec::new(),
                            access: BitAccess::new(lsb, msb),
                        })
                        .ok()?
                } else {
                    arena.alloc(SLTNode::Slice { expr, access }).ok()?
                }
            } else {
                arena.alloc(SLTNode::Slice { expr, access }).ok()?
            }
        }
        SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return None,
    };
    cache.insert(node, specialized);
    Some(specialized)
}

#[derive(Default)]
struct ProofBitCanonicalizer {
    bit_cache: HashMap<(NodeId, usize), NodeId>,
    /// Dense low-to-high mapping for each Concat. Building this once avoids
    /// rescanning an increasingly long part list independently for every bit.
    concat_layouts: HashMap<NodeId, Vec<(NodeId, usize)>>,
    #[cfg(test)]
    bit_cache_hits: usize,
    #[cfg(test)]
    bit_cache_misses: usize,
    #[cfg(test)]
    concat_layout_builds: usize,
}

impl ProofBitCanonicalizer {
    fn concat_bit_source(
        &mut self,
        node: NodeId,
        parts: &[(NodeId, usize)],
        bit: usize,
    ) -> Option<(NodeId, usize)> {
        #[cfg(test)]
        let mut built = false;
        let source = match self.concat_layouts.entry(node) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                entry.into_mut().get(bit).copied()
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                let total_width = parts.iter().try_fold(0usize, |width, (_, part_width)| {
                    width.checked_add(*part_width)
                })?;
                let mut layout = Vec::new();
                layout.try_reserve_exact(total_width).ok()?;
                for &(part, part_width) in parts.iter().rev() {
                    layout.extend((0..part_width).map(|part_bit| (part, part_bit)));
                }
                debug_assert_eq!(layout.len(), total_width);
                #[cfg(test)]
                {
                    built = true;
                }
                entry.insert(layout).get(bit).copied()
            }
        };
        #[cfg(test)]
        if built {
            self.concat_layout_builds += 1;
        }
        source
    }
}

fn proof_outputs_match(
    lhs: &[NodeId],
    rhs: &[NodeId],
    arena: &mut SLTNodeArena<VarId>,
    canonicalizer: &mut ProofBitCanonicalizer,
) -> Option<bool> {
    // A constant-index LHS is represented as RangeStore slices/concats, while
    // its dynamic template is a full-width mask/shift read-modify-write. Keep
    // the proof exact by comparing their per-bit canonical forms. Operations
    // that are not safely bit-decomposable remain opaque slices.
    if lhs == rhs {
        return Some(true);
    }
    if lhs.len() != rhs.len() {
        return Some(false);
    }

    for (&lhs, &rhs) in lhs.iter().zip(rhs) {
        let width = get_width(lhs, arena);
        if width != get_width(rhs, arena) {
            return Some(false);
        }
        for bit in 0..width {
            let lhs = canonicalize_proof_bit(lhs, bit, arena, canonicalizer)?;
            let rhs = canonicalize_proof_bit(rhs, bit, arena, canonicalizer)?;
            if lhs != rhs {
                return Some(false);
            }
        }
    }
    Some(true)
}

fn canonicalize_proof_bit(
    node: NodeId,
    bit: usize,
    arena: &mut SLTNodeArena<VarId>,
    canonicalizer: &mut ProofBitCanonicalizer,
) -> Option<NodeId> {
    if let Some(&canonical) = canonicalizer.bit_cache.get(&(node, bit)) {
        #[cfg(test)]
        {
            canonicalizer.bit_cache_hits += 1;
        }
        return Some(canonical);
    }
    #[cfg(test)]
    {
        canonicalizer.bit_cache_misses += 1;
    }
    let width = get_width(node, arena);
    if bit >= width {
        return None;
    }

    let canonical = match arena.get(node).clone() {
        SLTNode::Input {
            variable,
            index,
            access,
            ..
        } if index.is_empty() => arena
            .alloc(SLTNode::Input {
                variable,
                signed: false,
                index,
                access: BitAccess::new(access.lsb.checked_add(bit)?, access.lsb.checked_add(bit)?),
            })
            .ok()?,
        SLTNode::Constant(value, mask, _, _) => arena
            .alloc(SLTNode::Constant(
                (&value >> bit) & BigUint::from(1u8),
                (&mask >> bit) & BigUint::from(1u8),
                1,
                false,
            ))
            .ok()?,
        SLTNode::Binary(lhs, op @ (BinaryOp::And | BinaryOp::Or | BinaryOp::Xor), rhs)
            if bit < get_width(lhs, arena) && bit < get_width(rhs, arena) =>
        {
            let lhs = canonicalize_proof_bit(lhs, bit, arena, canonicalizer)?;
            let rhs = canonicalize_proof_bit(rhs, bit, arena, canonicalizer)?;
            alloc_proof_bit_binary(lhs, op, rhs, arena)?
        }
        SLTNode::Binary(lhs, op @ (BinaryOp::Shl | BinaryOp::Shr), rhs) => {
            let Some(shift) = specialized_constant(rhs, arena)
                .filter(|constant| constant.mask.is_zero())
                .and_then(|constant| constant.value.to_usize())
            else {
                return alloc_opaque_proof_bit(node, bit, arena, canonicalizer);
            };
            let source_bit = match op {
                BinaryOp::Shl => bit.checked_sub(shift),
                BinaryOp::Shr => bit.checked_add(shift),
                _ => unreachable!(),
            };
            match source_bit.filter(|&source_bit| source_bit < get_width(lhs, arena)) {
                Some(source_bit) => canonicalize_proof_bit(lhs, source_bit, arena, canonicalizer)?,
                None => alloc_proof_bit_constant(false, arena)?,
            }
        }
        SLTNode::Unary(UnaryOp::BitNot, inner) if bit < get_width(inner, arena) => {
            let inner = canonicalize_proof_bit(inner, bit, arena, canonicalizer)?;
            alloc_proof_bit_not(inner, arena)?
        }
        SLTNode::Mux {
            cond,
            then_expr,
            else_expr,
        } if get_width(cond, arena) == 1
            && bit < get_width(then_expr, arena)
            && bit < get_width(else_expr, arena) =>
        {
            let cond = canonicalize_proof_bit(cond, 0, arena, canonicalizer)?;
            let then_expr = canonicalize_proof_bit(then_expr, bit, arena, canonicalizer)?;
            let else_expr = canonicalize_proof_bit(else_expr, bit, arena, canonicalizer)?;
            if then_expr == else_expr {
                then_expr
            } else if let Some(value) = known_proof_bit(cond, arena) {
                if value { then_expr } else { else_expr }
            } else {
                arena
                    .alloc(SLTNode::Mux {
                        cond,
                        then_expr,
                        else_expr,
                    })
                    .ok()?
            }
        }
        SLTNode::Concat(parts) => {
            let (part, part_bit) = canonicalizer.concat_bit_source(node, &parts, bit)?;
            canonicalize_proof_bit(part, part_bit, arena, canonicalizer)?
        }
        SLTNode::Slice { expr, access } => {
            canonicalize_proof_bit(expr, access.lsb.checked_add(bit)?, arena, canonicalizer)?
        }
        SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return None,
        _ => return alloc_opaque_proof_bit(node, bit, arena, canonicalizer),
    };
    canonicalizer.bit_cache.insert((node, bit), canonical);
    Some(canonical)
}

fn alloc_opaque_proof_bit(
    node: NodeId,
    bit: usize,
    arena: &mut SLTNodeArena<VarId>,
    canonicalizer: &mut ProofBitCanonicalizer,
) -> Option<NodeId> {
    let canonical = if get_width(node, arena) == 1 {
        node
    } else {
        arena
            .alloc(SLTNode::Slice {
                expr: node,
                access: BitAccess::new(bit, bit),
            })
            .ok()?
    };
    canonicalizer.bit_cache.insert((node, bit), canonical);
    Some(canonical)
}

fn alloc_proof_bit_constant(value: bool, arena: &mut SLTNodeArena<VarId>) -> Option<NodeId> {
    arena
        .alloc(SLTNode::Constant(
            BigUint::from(u8::from(value)),
            BigUint::from(0u8),
            1,
            false,
        ))
        .ok()
}

fn known_proof_bit(node: NodeId, arena: &SLTNodeArena<VarId>) -> Option<bool> {
    let constant = specialized_constant(node, arena)?;
    (constant.width == 1 && constant.mask.is_zero()).then(|| !constant.value.is_zero())
}

fn alloc_proof_bit_binary(
    lhs: NodeId,
    op: BinaryOp,
    rhs: NodeId,
    arena: &mut SLTNodeArena<VarId>,
) -> Option<NodeId> {
    let lhs_known = known_proof_bit(lhs, arena);
    let rhs_known = known_proof_bit(rhs, arena);
    match op {
        BinaryOp::And => {
            if lhs_known == Some(false) || rhs_known == Some(false) {
                return alloc_proof_bit_constant(false, arena);
            }
            if lhs_known == Some(true) {
                return Some(rhs);
            }
            if rhs_known == Some(true) {
                return Some(lhs);
            }
        }
        BinaryOp::Or => {
            if lhs_known == Some(true) || rhs_known == Some(true) {
                return alloc_proof_bit_constant(true, arena);
            }
            if lhs_known == Some(false) {
                return Some(rhs);
            }
            if rhs_known == Some(false) {
                return Some(lhs);
            }
        }
        BinaryOp::Xor => {}
        _ => return None,
    }
    arena.alloc(SLTNode::Binary(lhs, op, rhs)).ok()
}

fn alloc_proof_bit_not(inner: NodeId, arena: &mut SLTNodeArena<VarId>) -> Option<NodeId> {
    if let Some(value) = known_proof_bit(inner, arena) {
        alloc_proof_bit_constant(!value, arena)
    } else {
        arena.alloc(SLTNode::Unary(UnaryOp::BitNot, inner)).ok()
    }
}

fn specialized_constant(node: NodeId, arena: &SLTNodeArena<VarId>) -> Option<SpecializedConstant> {
    let SLTNode::Constant(value, mask, width, signed) = arena.get(node) else {
        return None;
    };
    Some(SpecializedConstant {
        value: value.clone(),
        mask: mask.clone(),
        width: *width,
        signed: *signed,
    })
}

fn specialized_binary_result_signed(
    op: BinaryOp,
    lhs: &SpecializedConstant,
    rhs: &SpecializedConstant,
) -> bool {
    match op {
        BinaryOp::Eq
        | BinaryOp::Ne
        | BinaryOp::EqWildcard
        | BinaryOp::NeWildcard
        | BinaryOp::LtU
        | BinaryOp::LtS
        | BinaryOp::LeU
        | BinaryOp::LeS
        | BinaryOp::GtU
        | BinaryOp::GtS
        | BinaryOp::GeU
        | BinaryOp::GeS
        | BinaryOp::LogicAnd
        | BinaryOp::LogicOr
        | BinaryOp::DivU
        | BinaryOp::RemU => false,
        BinaryOp::DivS | BinaryOp::RemS => true,
        BinaryOp::Shl | BinaryOp::Shr | BinaryOp::Sar => lhs.signed,
        BinaryOp::Add
        | BinaryOp::Sub
        | BinaryOp::Mul
        | BinaryOp::And
        | BinaryOp::Or
        | BinaryOp::Xor => lhs.signed && rhs.signed,
    }
}

fn specialized_unary_result_signed(op: UnaryOp, inner: &SpecializedConstant) -> bool {
    match op {
        UnaryOp::Minus => true,
        UnaryOp::Ident | UnaryOp::BitNot => inner.signed,
        UnaryOp::LogicNot
        | UnaryOp::And
        | UnaryOp::Or
        | UnaryOp::Xor
        | UnaryOp::PopCount
        | UnaryOp::CountLeadingZeros
        | UnaryOp::CountTrailingZeros => false,
    }
}

fn zero_extend_specialized(
    node: NodeId,
    target_width: usize,
    arena: &mut SLTNodeArena<VarId>,
) -> Option<NodeId> {
    let width = get_width(node, arena);
    if width == target_width {
        return Some(node);
    }
    if width == 0 || width > target_width {
        return None;
    }
    if let Some(mut constant) = specialized_constant(node, arena) {
        constant.width = target_width;
        constant.signed = false;
        return alloc_specialized_constant(constant, arena);
    }
    let padding_width = target_width.checked_sub(width)?;
    let padding = arena
        .alloc(SLTNode::Constant(
            BigUint::from(0u8),
            BigUint::from(0u8),
            padding_width,
            false,
        ))
        .ok()?;
    arena
        .alloc(SLTNode::Concat(vec![
            (padding, padding_width),
            (node, width),
        ]))
        .ok()
}

fn alloc_specialized_constant(
    constant: SpecializedConstant,
    arena: &mut SLTNodeArena<VarId>,
) -> Option<NodeId> {
    arena
        .alloc(SLTNode::Constant(
            constant.value & width_mask(constant.width),
            constant.mask & width_mask(constant.width),
            constant.width,
            false,
        ))
        .ok()
}

fn specialize_concat_constant(
    parts: &[(NodeId, usize)],
    arena: &SLTNodeArena<VarId>,
) -> Option<SpecializedConstant> {
    let mut value = BigUint::from(0u8);
    let mut mask = BigUint::from(0u8);
    let mut total_width = 0usize;
    for &(part, width) in parts {
        let part = specialized_constant(part, arena)?;
        if part.width != width {
            return None;
        }
        value = (value << width) | (part.value & width_mask(width));
        mask = (mask << width) | (part.mask & width_mask(width));
        total_width = total_width.checked_add(width)?;
    }
    Some(SpecializedConstant {
        value,
        mask,
        width: total_width,
        signed: false,
    })
}

fn specialize_binary_constant(
    op: BinaryOp,
    lhs: &SpecializedConstant,
    rhs: &SpecializedConstant,
    result_width: usize,
    result_signed: bool,
) -> Option<SpecializedConstant> {
    if !lhs.mask.is_zero() || !rhs.mask.is_zero() || result_width == 0 {
        return None;
    }
    let mask = width_mask(result_width);
    let shift = rhs.value.to_usize();
    let bool_value = |value: bool| BigUint::from(u8::from(value));
    let value = match op {
        BinaryOp::Add => (&lhs.value + &rhs.value) & &mask,
        BinaryOp::Sub => {
            let modulus = BigUint::from(1u8) << result_width;
            (&modulus + &lhs.value - (&rhs.value & &mask)) & &mask
        }
        BinaryOp::Mul => (&lhs.value * &rhs.value) & &mask,
        BinaryOp::DivU => {
            if rhs.value.is_zero() {
                return None;
            }
            (&lhs.value / &rhs.value) & &mask
        }
        BinaryOp::RemU => {
            if rhs.value.is_zero() {
                return None;
            }
            (&lhs.value % &rhs.value) & &mask
        }
        BinaryOp::DivS => {
            let divisor = bits_to_signed(&rhs.value, rhs.width);
            if divisor.is_zero() {
                return None;
            }
            signed_to_bits(
                bits_to_signed(&lhs.value, lhs.width) / divisor,
                result_width,
            )?
        }
        BinaryOp::RemS => {
            let divisor = bits_to_signed(&rhs.value, rhs.width);
            if divisor.is_zero() {
                return None;
            }
            signed_to_bits(
                bits_to_signed(&lhs.value, lhs.width) % divisor,
                result_width,
            )?
        }
        BinaryOp::And => (&lhs.value & &rhs.value) & &mask,
        BinaryOp::Or => (&lhs.value | &rhs.value) & &mask,
        BinaryOp::Xor => (&lhs.value ^ &rhs.value) & &mask,
        BinaryOp::Shl => match shift {
            Some(shift) if shift < result_width => (&lhs.value << shift) & &mask,
            Some(_) => BigUint::from(0u8),
            None => return None,
        },
        BinaryOp::Shr => match shift {
            Some(shift) if shift < lhs.width => (&lhs.value >> shift) & &mask,
            Some(_) => BigUint::from(0u8),
            None => return None,
        },
        BinaryOp::Sar => match shift {
            Some(shift) => {
                signed_to_bits(bits_to_signed(&lhs.value, lhs.width) >> shift, result_width)?
            }
            None => return None,
        },
        BinaryOp::Eq | BinaryOp::EqWildcard => bool_value(lhs.value == rhs.value),
        BinaryOp::Ne | BinaryOp::NeWildcard => bool_value(lhs.value != rhs.value),
        BinaryOp::LtU => bool_value(lhs.value < rhs.value),
        BinaryOp::LeU => bool_value(lhs.value <= rhs.value),
        BinaryOp::GtU => bool_value(lhs.value > rhs.value),
        BinaryOp::GeU => bool_value(lhs.value >= rhs.value),
        BinaryOp::LtS => bool_value(
            bits_to_signed(&lhs.value, lhs.width) < bits_to_signed(&rhs.value, rhs.width),
        ),
        BinaryOp::LeS => bool_value(
            bits_to_signed(&lhs.value, lhs.width) <= bits_to_signed(&rhs.value, rhs.width),
        ),
        BinaryOp::GtS => bool_value(
            bits_to_signed(&lhs.value, lhs.width) > bits_to_signed(&rhs.value, rhs.width),
        ),
        BinaryOp::GeS => bool_value(
            bits_to_signed(&lhs.value, lhs.width) >= bits_to_signed(&rhs.value, rhs.width),
        ),
        BinaryOp::LogicAnd => bool_value(!lhs.value.is_zero() && !rhs.value.is_zero()),
        BinaryOp::LogicOr => bool_value(!lhs.value.is_zero() || !rhs.value.is_zero()),
    };
    Some(SpecializedConstant {
        value: value & mask,
        mask: BigUint::from(0u8),
        width: result_width,
        signed: !matches!(
            op,
            BinaryOp::Eq
                | BinaryOp::Ne
                | BinaryOp::EqWildcard
                | BinaryOp::NeWildcard
                | BinaryOp::LtU
                | BinaryOp::LtS
                | BinaryOp::LeU
                | BinaryOp::LeS
                | BinaryOp::GtU
                | BinaryOp::GtS
                | BinaryOp::GeU
                | BinaryOp::GeS
                | BinaryOp::LogicAnd
                | BinaryOp::LogicOr
        ) && result_signed,
    })
}

fn specialize_unary_constant(
    op: UnaryOp,
    inner: &SpecializedConstant,
    result_width: usize,
    result_signed: bool,
) -> Option<SpecializedConstant> {
    if !inner.mask.is_zero() || result_width == 0 {
        return None;
    }
    let result_mask = width_mask(result_width);
    let value = match op {
        UnaryOp::Ident => inner.value.clone(),
        UnaryOp::Minus => {
            let modulus = BigUint::from(1u8) << result_width;
            (&modulus - (&inner.value & &result_mask)) & &result_mask
        }
        UnaryOp::BitNot => &result_mask ^ (&inner.value & &result_mask),
        UnaryOp::LogicNot => BigUint::from(u8::from(inner.value.is_zero())),
        UnaryOp::And => BigUint::from(u8::from(
            (&inner.value & width_mask(inner.width)) == width_mask(inner.width),
        )),
        UnaryOp::Or => BigUint::from(u8::from(!inner.value.is_zero())),
        UnaryOp::Xor => BigUint::from(
            inner
                .value
                .iter_u64_digits()
                .map(u64::count_ones)
                .sum::<u32>()
                & 1,
        ),
        UnaryOp::PopCount => BigUint::from(
            inner
                .value
                .iter_u64_digits()
                .map(u64::count_ones)
                .sum::<u32>(),
        ),
        UnaryOp::CountLeadingZeros => {
            BigUint::from(inner.width.saturating_sub(inner.value.bits() as usize))
        }
        UnaryOp::CountTrailingZeros => {
            let zeros = inner
                .value
                .iter_u64_digits()
                .enumerate()
                .find_map(|(index, digit)| {
                    (digit != 0)
                        .then_some(index * u64::BITS as usize + digit.trailing_zeros() as usize)
                })
                .unwrap_or(inner.width)
                .min(inner.width);
            BigUint::from(zeros)
        }
    };
    Some(SpecializedConstant {
        value: value & result_mask,
        mask: BigUint::from(0u8),
        width: result_width,
        signed: matches!(op, UnaryOp::Ident | UnaryOp::Minus | UnaryOp::BitNot) && result_signed,
    })
}

fn width_mask(width: usize) -> BigUint {
    if width == 0 {
        BigUint::from(0u8)
    } else {
        (BigUint::from(1u8) << width) - BigUint::from(1u8)
    }
}

fn bits_to_signed(value: &BigUint, width: usize) -> BigInt {
    let value = value & width_mask(width);
    if width != 0 && ((&value >> (width - 1)) & BigUint::from(1u8)) == BigUint::from(1u8) {
        BigInt::from_biguint(Sign::Plus, value)
            - BigInt::from_biguint(Sign::Plus, BigUint::from(1u8) << width)
    } else {
        BigInt::from_biguint(Sign::Plus, value)
    }
}

fn signed_to_bits(value: BigInt, width: usize) -> Option<BigUint> {
    let modulus = BigInt::from_biguint(Sign::Plus, BigUint::from(1u8) << width);
    let normalized = ((value % &modulus) + &modulus) % &modulus;
    normalized.to_biguint()
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
            rewrite_index(&mut destination.index, loop_var, mode).is_some()
                && rewrite_select(&mut destination.select, loop_var, mode).is_some()
                && rewrite_expression(&mut assign.expr, loop_var, mode).is_some()
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

fn rewrite_index(index: &mut VarIndex, loop_var: VarId, mode: &RewriteMode) -> Option<bool> {
    let mut depends = false;
    for expression in &mut index.0 {
        depends |= rewrite_expression(expression, loop_var, mode)?;
    }
    Some(depends)
}

fn rewrite_select(select: &mut VarSelect, loop_var: VarId, mode: &RewriteMode) -> Option<bool> {
    let mut depends = false;
    for expression in &mut select.0 {
        depends |= rewrite_expression(expression, loop_var, mode)?;
    }
    if let Some((_, expression)) = &mut select.1 {
        depends |= rewrite_expression(expression, loop_var, mode)?;
    }
    Some(depends)
}

fn rewrite_expression(
    expression: &mut Expression,
    loop_var: VarId,
    mode: &RewriteMode,
) -> Option<bool> {
    let depends = match expression {
        Expression::Term(factor) => match factor.as_mut() {
            Factor::Variable(variable, index, select, comptime) => {
                let index_depends = rewrite_index(index, loop_var, mode)?;
                let select_depends = rewrite_select(select, loop_var, mode)?;
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
    use veryl_analyzer::ir::{Component, Declaration, Ir, VarKind, VarPath};
    use veryl_analyzer::{Analyzer, Context, attribute_table, symbol_table};
    use veryl_metadata::Metadata;
    use veryl_parser::Parser;
    use veryl_parser::resource_table;

    use super::*;
    use crate::logic_tree::comb::parse_comb_with_loop_recovery;
    use crate::parser::loop_provenance::{LoopProvenance, LoopSourceTable};

    #[test]
    fn constant_specialization_distinguishes_signed_division_and_remainder() {
        let lhs = SpecializedConstant {
            value: BigUint::from(0xf9u8),
            mask: BigUint::from(0u8),
            width: 8,
            signed: true,
        };
        let rhs = SpecializedConstant {
            value: BigUint::from(4u8),
            mask: BigUint::from(0u8),
            width: 8,
            signed: true,
        };

        let div_u = specialize_binary_constant(BinaryOp::DivU, &lhs, &rhs, 8, false).unwrap();
        let rem_u = specialize_binary_constant(BinaryOp::RemU, &lhs, &rhs, 8, false).unwrap();
        let div_s = specialize_binary_constant(BinaryOp::DivS, &lhs, &rhs, 8, true).unwrap();
        let rem_s = specialize_binary_constant(BinaryOp::RemS, &lhs, &rhs, 8, true).unwrap();

        assert_eq!(div_u.value, BigUint::from(0x3eu8));
        assert_eq!(rem_u.value, BigUint::from(1u8));
        assert_eq!(div_s.value, BigUint::from(0xffu8));
        assert_eq!(rem_s.value, BigUint::from(0xfdu8));
        assert!(!div_u.signed && !rem_u.signed);
        assert!(div_s.signed && rem_s.signed);
    }

    fn analyze(code: &str) -> (Module, LoopProvenance) {
        symbol_table::clear();
        attribute_table::clear();

        let metadata = Metadata::create_default("prj").expect("default metadata must be valid");
        let analyzer = Analyzer::new(&metadata);
        let parsed = Parser::parse(code, &"").expect("test source must parse");
        let loop_sources = LoopSourceTable::collect([&parsed.veryl]);
        let pass1_errors = analyzer.analyze_pass1("prj", &parsed.veryl);
        assert!(pass1_errors.is_empty(), "pass1 errors: {pass1_errors:?}");
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

    const CROSS_STATE_LOOP: &str = r#"
        module Top (
            value: output logic<8>,
        ) {
            var left : logic<4>;
            var right: logic<4>;
            always_comb {
                left  = 4'd1;
                right = 4'd2;
                for i in 0..4 {
                    let old_left : logic<4> = left;
                    let old_right: logic<4> = right;
                    left  = old_right + (i as 4);
                    right = old_left ^ (i as 4);
                }
                value = {left, right};
            }
        }
    "#;

    const PARTIAL_FOUND_LOOP: &str = r#"
        module Top (
            source: input  logic<4>,
            mask  : input  logic<4>,
            old   : input  logic<4>,
            bound : input  logic<3>,
            vm    : input  logic,
            mode  : input  logic<2>,
            value : output logic<5>,
        ) {
            var found   : logic;
            var selected: logic<4>;
            always_comb {
                found    = 1'b0;
                selected = old;
                for i in 0..4 {
                    if ((i as 3) <: bound) && (vm || mask[i]) {
                        let bf : logic = !found && !source[i];
                        let of : logic = !found && source[i];
                        let sif: logic = !found;
                        selected[i] = case mode {
                            2'b01  : bf,
                            2'b10  : of,
                            default: sif,
                        };
                        found = found || source[i];
                    }
                }
                value = {found, selected};
            }
        }
    "#;

    const MULTIPLE_PARTIAL_WRITES_LOOP: &str = r#"
        module Top (
            low  : input  logic<4>,
            high : input  logic<4>,
            value: output logic<8>,
        ) {
            var state: logic<2> [4];
            always_comb {
                state[0] = 2'b00;
                state[1] = 2'b00;
                state[2] = 2'b00;
                state[3] = 2'b00;
                for i in 0..4 {
                    state[i][0] = low[i];
                    state[i][1] = high[i];
                }
                value = {state[3], state[2], state[1], state[0]};
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
    fn whole_fold_proof_composes_cross_state_updates() {
        let (module, provenance) = analyze(CROSS_STATE_LOOP);
        let candidates = provenance.candidates_for_module(&module);
        assert_eq!(candidates.len(), 1);
        let declaration = module
            .declarations
            .iter()
            .find_map(|declaration| match declaration {
                Declaration::Comb(comb) => Some(comb),
                _ => None,
            })
            .unwrap();
        let start = declaration
            .statements
            .iter()
            .position(|statement| statement_in_range(statement, candidates[0].source.body_token))
            .unwrap();
        let run_len = candidate_run_len(
            &declaration.statements[start..],
            candidates[0].source.body_token,
        );
        let proof_variables =
            proof_variable_ids(&declaration.statements[start..start + run_len], &[]).unwrap();
        assert!(proof_variables.contains(&variable(&module, "left")));
        assert!(proof_variables.contains(&variable(&module, "right")));
        assert!(!proof_variables.contains(&variable(&module, "value")));
        assert!(proof_variables.len() < module.variables.len());
        assert_eq!(
            marker_store(&module, &proof_variables).unwrap().len(),
            proof_variables.len()
        );

        let (_, arena) = parse_with_candidates(&module, &candidates);

        let states = arena
            .iter()
            .find_map(|node| match node {
                SLTNode::ForFoldGroup { states, .. } => Some(states),
                _ => None,
            })
            .expect("cross-state loop must pass whole-fold composition proof");
        assert_eq!(states.len(), 2);
        assert_eq!(
            states
                .iter()
                .map(|state| state.target.id)
                .collect::<HashSet<_>>(),
            [variable(&module, "left"), variable(&module, "right")]
                .into_iter()
                .collect()
        );
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
        let all_variables = module.variables.keys().copied().collect::<HashSet<_>>();
        let mut initial_store = marker_store(&module, &all_variables).unwrap();
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
    fn recovers_loop_dependent_destination_as_full_width_state() {
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
        let states = arena
            .iter()
            .find_map(|node| match node {
                SLTNode::ForFoldGroup { states, .. } => Some(states),
                _ => None,
            })
            .expect("dynamic partial write must be recovered");
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].target.id, variable(&module, "state"));
        assert_eq!(states[0].target.access, BitAccess::new(0, 3));
    }

    #[test]
    fn recovers_partial_dynamic_write_with_cross_state_found_recurrence() {
        let (module, provenance) = analyze(PARTIAL_FOUND_LOOP);
        let candidates = provenance.candidates_for_module(&module);
        let (_, arena) = parse_with_candidates(&module, &candidates);
        let (group, states, loop_width, loop_signed, start, step, trip_count) = arena
            .iter()
            .enumerate()
            .find_map(|(index, node)| match node {
                SLTNode::ForFoldGroup {
                    loop_width,
                    loop_signed,
                    start,
                    step,
                    trip_count,
                    states,
                    ..
                } => Some((
                    NodeId(index),
                    states,
                    *loop_width,
                    *loop_signed,
                    start,
                    step,
                    *trip_count,
                )),
                _ => None,
            })
            .expect("cross-state partial writes must be recovered");

        assert_eq!(loop_width, 32);
        assert!(loop_signed, "Veryl's recovered for-loop IV is signed");
        assert_eq!(start, &BigInt::from(0u8));
        assert_eq!(step, &BigInt::from(1u8));
        assert_eq!(trip_count, 4);
        assert_eq!(states.len(), 2);
        let targets = states
            .iter()
            .map(|state| (state.target.id, state.target.access))
            .collect::<HashMap<_, _>>();
        assert_eq!(
            targets.get(&variable(&module, "found")),
            Some(&BitAccess::new(0, 0))
        );
        assert_eq!(
            targets.get(&variable(&module, "selected")),
            Some(&BitAccess::new(0, 3))
        );
        assert!(
            crate::logic_tree::matches_slt_or_scan_group(group, &arena),
            "the exact recovered Veryl scan must enter the word-scan lowering"
        );
    }

    #[test]
    fn deduplicates_multiple_partial_writes_to_the_same_base_state() {
        let (module, provenance) = analyze(MULTIPLE_PARTIAL_WRITES_LOOP);
        let candidates = provenance.candidates_for_module(&module);
        let (_, arena) = parse_with_candidates(&module, &candidates);
        let states = arena
            .iter()
            .find_map(|node| match node {
                SLTNode::ForFoldGroup { states, .. } => Some(states),
                _ => None,
            })
            .expect("multiple writes to one base must be recovered");

        assert_eq!(states.len(), 1);
        assert_eq!(states[0].target.id, variable(&module, "state"));
        assert_eq!(states[0].target.access, BitAccess::new(0, 7));
    }

    #[test]
    fn rejects_wrong_affine_provenance_for_partial_dynamic_writes() {
        let (module, provenance) = analyze(PARTIAL_FOUND_LOOP);
        let mut candidates = provenance.candidates_for_module(&module);
        for (iteration, wrong_value) in candidates[0]
            .unrolled
            .iterations
            .iter_mut()
            .zip([3, 2, 1, 0])
        {
            iteration.value = wrong_value;
        }

        let (_, arena) = parse_with_candidates(&module, &candidates);
        assert_eq!(group_count(&arena), 0);
    }

    #[test]
    fn recovers_heliodor_shaped_32_iteration_array_fold() {
        let code = r#"
            module Top (
                en        : input  logic,
                load_age  : input  logic<5>,
                load_addr : input  logic<64>,
                store_addr: input  logic<64> [32],
                store_data: input  logic<64> [32],
                store_size: input  logic<2>  [32],
                store_fwd : input  logic     [32],
                value     : output logic<8>,
            ) {
                var found: logic;
                var age  : logic<5>;
                var selected: logic<8>;

                always_comb {
                    found = 1'b0;
                    age   = 5'd0;
                    selected = 8'd0;
                    if en {
                        for i in 0..32 {
                            let age_i: logic<5>  = (i as 5);
                            let sa   : logic<64> = store_addr[i];
                            let la   : logic<64> = load_addr;
                            let cov  : logic = store_fwd[i]
                                && (age_i <: load_age)
                                && (la >= sa)
                                && (la <: sa + ((5'd1 << store_size[i]) as 64));
                            let off  : logic<64> = la - sa;
                            let byt  : logic<64> = store_data[i] >> {off[2:0], 3'b000};
                            if cov && (!found || age_i >: age) {
                                found = 1'b1;
                                age   = age_i;
                                selected = byt[7:0];
                            }
                        }
                    }
                    value = selected;
                }
            }
        "#;
        let (module, provenance) = analyze(code);
        let candidates = provenance.candidates_for_module(&module);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].unrolled.iterations.len(), 32);
        for iteration in &candidates[0].unrolled.iterations {
            let local_lets = module
                .variables
                .values()
                .filter(|variable| {
                    variable.kind == VarKind::Let
                        && variable.path.0.len() > iteration.hierarchy.len()
                        && variable.path.0.starts_with(&iteration.hierarchy)
                })
                .count();
            assert_eq!(local_lets, 6);
        }

        let (_, arena) = parse_with_candidates(&module, &candidates);
        assert_eq!(group_count(&arena), 1);
        let (loop_var, states, trip_count) = arena
            .iter()
            .find_map(|node| match node {
                SLTNode::ForFoldGroup {
                    loop_var,
                    states,
                    trip_count,
                    ..
                } => Some((*loop_var, states, *trip_count)),
                _ => None,
            })
            .expect("recovered group must exist");
        assert_eq!(trip_count, 32);
        assert_eq!(states.len(), 3);

        let mut narrow_widths = BTreeSet::new();
        let mut visited = HashSet::default();
        for state in states {
            collect_narrow_dynamic_input_widths(
                state.update,
                loop_var,
                &arena,
                &mut visited,
                &mut narrow_widths,
            );
        }
        assert!(narrow_widths.contains(&1), "missing scalar dynamic load");
        assert!(narrow_widths.contains(&2), "missing size dynamic load");
        assert!(narrow_widths.contains(&64), "missing 64-bit dynamic load");
    }

    #[test]
    fn proof_bit_canonicalizer_reuses_bits_and_concat_layouts() {
        let (module, _) = analyze(MULTI_STATE_LOOP);
        let bits = variable(&module, "bits");
        let mut arena = SLTNodeArena::new();
        let input = arena
            .alloc(SLTNode::Input {
                variable: bits,
                signed: false,
                index: Vec::new(),
                access: BitAccess::new(0, 3),
            })
            .unwrap();
        let low = arena
            .alloc(SLTNode::Slice {
                expr: input,
                access: BitAccess::new(0, 1),
            })
            .unwrap();
        let high = arena
            .alloc(SLTNode::Slice {
                expr: input,
                access: BitAccess::new(2, 3),
            })
            .unwrap();
        let concat = arena
            .alloc(SLTNode::Concat(vec![(high, 2), (low, 2)]))
            .unwrap();
        let mut canonicalizer = ProofBitCanonicalizer::default();

        assert_eq!(
            proof_outputs_match(&[concat], &[input], &mut arena, &mut canonicalizer),
            Some(true)
        );
        assert_eq!(canonicalizer.concat_layout_builds, 1);
        assert_eq!(
            canonicalizer.concat_layouts[&concat],
            vec![(low, 0), (low, 1), (high, 0), (high, 1)]
        );
        let first_misses = canonicalizer.bit_cache_misses;
        let first_hits = canonicalizer.bit_cache_hits;
        let first_nodes = arena.len();

        // The second comparison must be served entirely by the shared bit
        // cache: four lhs and four rhs root lookups, with no new proof nodes.
        assert_eq!(
            proof_outputs_match(&[concat], &[input], &mut arena, &mut canonicalizer),
            Some(true)
        );
        assert_eq!(canonicalizer.bit_cache_misses, first_misses);
        assert_eq!(canonicalizer.bit_cache_hits - first_hits, 8);
        assert_eq!(canonicalizer.concat_layout_builds, 1);
        assert_eq!(arena.len(), first_nodes);

        // Force bit recomputation while retaining the Concat layout. The
        // layout must still be reused rather than rebuilt or rescanned.
        canonicalizer.bit_cache.clear();
        assert_eq!(
            proof_outputs_match(&[concat], &[input], &mut arena, &mut canonicalizer),
            Some(true)
        );
        assert_eq!(canonicalizer.concat_layout_builds, 1);
        assert_eq!(arena.len(), first_nodes);
    }

    #[test]
    fn known_mux_specialization_preserves_result_width() {
        let mut arena = SLTNodeArena::new();
        let cond = arena
            .alloc(SLTNode::Constant(
                BigUint::from(1u8),
                BigUint::from(0u8),
                1,
                false,
            ))
            .unwrap();
        let narrow = arena
            .alloc(SLTNode::Constant(
                BigUint::from(5u8),
                BigUint::from(0u8),
                3,
                true,
            ))
            .unwrap();
        let wide = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0xa5u8),
                BigUint::from(0u8),
                8,
                false,
            ))
            .unwrap();
        let mux = arena
            .alloc(SLTNode::Mux {
                cond,
                then_expr: narrow,
                else_expr: wide,
            })
            .unwrap();

        let specialized =
            specialize_slt_node(mux, None, None, &mut arena, &mut HashMap::default()).unwrap();
        assert!(matches!(
            arena.get(specialized),
            SLTNode::Constant(value, mask, 8, false)
                if value == &BigUint::from(5u8) && mask.is_zero()
        ));
    }

    #[test]
    fn specializes_dynamic_slt_for_every_iteration() {
        let (module, provenance) = analyze(MULTI_STATE_LOOP);
        let candidate = provenance
            .candidates_for_module(&module)
            .into_iter()
            .next()
            .unwrap();
        let loop_var = candidate.unrolled.iterations[0].loop_var;
        let bits = variable(&module, "bits");
        let mut arena = SLTNodeArena::new();
        let loop_input = arena
            .alloc(SLTNode::Input {
                variable: loop_var,
                signed: true,
                index: Vec::new(),
                access: BitAccess::new(0, 31),
            })
            .unwrap();
        let raw_bits = arena
            .alloc(SLTNode::Input {
                variable: bits,
                signed: true,
                index: vec![super::super::SLTIndex {
                    node: loop_input,
                    stride: 1,
                }],
                access: BitAccess::new(0, 3),
            })
            .unwrap();
        let dynamic_bit = arena
            .alloc(SLTNode::Slice {
                expr: raw_bits,
                access: BitAccess::new(0, 0),
            })
            .unwrap();
        let narrow_loop_value = arena
            .alloc(SLTNode::Slice {
                expr: loop_input,
                access: BitAccess::new(0, 2),
            })
            .unwrap();

        for iteration in &candidate.unrolled.iterations {
            let mut cache = HashMap::default();
            let specialized_bit = specialize_slt_node(
                dynamic_bit,
                Some((loop_var, iteration.value)),
                None,
                &mut arena,
                &mut cache,
            )
            .unwrap();
            let expected_bit = arena
                .alloc(SLTNode::Input {
                    variable: bits,
                    signed: false,
                    index: Vec::new(),
                    access: BitAccess::new(iteration.value, iteration.value),
                })
                .unwrap();
            assert_eq!(specialized_bit, expected_bit);

            let mut cache = HashMap::default();
            let specialized_value = specialize_slt_node(
                narrow_loop_value,
                Some((loop_var, iteration.value)),
                None,
                &mut arena,
                &mut cache,
            )
            .unwrap();
            let expected_value = arena
                .alloc(SLTNode::Constant(
                    BigUint::from(iteration.value & 0b111),
                    BigUint::from(0u8),
                    3,
                    false,
                ))
                .unwrap();
            assert_eq!(specialized_value, expected_value);
        }

        let signed_constant = arena
            .alloc(SLTNode::Constant(
                BigUint::from(0xf5u8),
                BigUint::from(0u8),
                8,
                true,
            ))
            .unwrap();
        let signed_slice = arena
            .alloc(SLTNode::Slice {
                expr: signed_constant,
                access: BitAccess::new(0, 3),
            })
            .unwrap();
        let specialized_slice = specialize_slt_node(
            signed_slice,
            None,
            None,
            &mut arena,
            &mut HashMap::default(),
        )
        .unwrap();
        assert!(matches!(
            arena.get(specialized_slice),
            SLTNode::Constant(value, mask, 4, false)
                if value == &BigUint::from(5u8) && mask.is_zero()
        ));
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

    fn collect_narrow_dynamic_input_widths(
        root: NodeId,
        loop_var: VarId,
        arena: &SLTNodeArena<VarId>,
        visited: &mut HashSet<NodeId>,
        widths: &mut BTreeSet<usize>,
    ) {
        if !visited.insert(root) {
            return;
        }
        match arena.get(root) {
            SLTNode::Input { index, .. } => {
                for entry in index {
                    collect_narrow_dynamic_input_widths(
                        entry.node, loop_var, arena, visited, widths,
                    );
                }
            }
            SLTNode::Slice { expr, access } => {
                if let SLTNode::Input {
                    index,
                    access: raw_access,
                    ..
                } = arena.get(*expr)
                    && !index.is_empty()
                    && index.iter().any(|entry| {
                        let mut inputs = HashSet::default();
                        let mut index_visited = HashSet::default();
                        collect_template_inputs(entry.node, arena, &mut index_visited, &mut inputs)
                            && inputs.contains(&loop_var)
                    })
                {
                    let width = access.msb - access.lsb + 1;
                    let raw_width = raw_access.msb - raw_access.lsb + 1;
                    assert!(width < raw_width, "dynamic array read was not narrowed");
                    widths.insert(width);
                }
                collect_narrow_dynamic_input_widths(*expr, loop_var, arena, visited, widths);
            }
            SLTNode::Binary(lhs, _, rhs) => {
                collect_narrow_dynamic_input_widths(*lhs, loop_var, arena, visited, widths);
                collect_narrow_dynamic_input_widths(*rhs, loop_var, arena, visited, widths);
            }
            SLTNode::Unary(_, inner) => {
                collect_narrow_dynamic_input_widths(*inner, loop_var, arena, visited, widths)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                collect_narrow_dynamic_input_widths(*cond, loop_var, arena, visited, widths);
                collect_narrow_dynamic_input_widths(*then_expr, loop_var, arena, visited, widths);
                collect_narrow_dynamic_input_widths(*else_expr, loop_var, arena, visited, widths);
            }
            SLTNode::Concat(parts) => {
                for (part, _) in parts {
                    collect_narrow_dynamic_input_widths(*part, loop_var, arena, visited, widths);
                }
            }
            SLTNode::Constant(..) | SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => {}
        }
    }
}
