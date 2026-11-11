use std::collections::BTreeSet;

use num_bigint::{BigInt, BigUint, Sign};
use num_traits::{One, Signed, ToPrimitive, Zero};
use veryl_analyzer::ir::{
    ArrayLiteralItem, Comptime, Expression, Factor, Module, Op, Statement, ValueVariant, VarId,
    VarIndex, VarSelect,
};
use veryl_analyzer::value::Value;
use veryl_parser::token_range::TokenRange;

use super::{
    ActiveGuard, BoundaryMap, NodeId, SLTForFoldGroupState, SLTNode, SLTNodeArena, SymbolicStore,
    combine_active_guard, combine_parts_with_default, eval_expression_effectful, eval_statement,
    eval_statement_with_recovery, get_width, merge_boundaries, procedural_condition,
    range_store_error,
};
use crate::loop_provenance::{LoopRecoveryCandidate, UnrolledLoopCandidate};
use crate::{HashMap, HashSet, ParserError, function_call_arg, resolve_total_width};
use celox_design::{BinaryOp, BitAccess, UnaryOp, VarAtomBase};
use celox_slt::{LogicPath, RangeStore};

mod combinational;
mod folded_value;
mod parameterize;
mod proof_bits;
mod prove;
mod rewrite;
mod specialize;
pub(crate) use combinational::*;
use folded_value::*;
use parameterize::*;
use proof_bits::*;
use prove::*;
use rewrite::*;
use specialize::*;

pub(super) fn eval_statements(
    module: &Module,
    mut store: SymbolicStore<VarId>,
    mut boundaries: BoundaryMap<VarId>,
    statements: &[Statement],
    arena: &mut SLTNodeArena<VarId>,
    candidates: &[LoopRecoveryCandidate],
    active_guard: Option<&ActiveGuard>,
) -> Result<(SymbolicStore<VarId>, BoundaryMap<VarId>), ParserError> {
    let mut index = 0usize;
    while index < statements.len() {
        if let Statement::If(if_stmt) = &statements[index]
            && if_stmt.false_side.is_empty()
        {
            let guarded_matches = matching_candidates(module, &if_stmt.true_side, candidates);
            if guarded_matches.len() == 1 {
                let candidate = guarded_matches[0];
                let store_before_probe = store.fork();
                let ((guard, guard_sources), guard_boundaries) =
                    eval_expression_effectful(module, &mut store, &if_stmt.cond, arena, None)?;
                let guard = procedural_condition(arena, guard)?;
                let guard = combine_active_guard(arena, active_guard, guard, &guard_sources)?;
                if let Some((next_store, recovered_boundaries)) = recover_group(
                    module,
                    &store,
                    &if_stmt.true_side,
                    candidate,
                    Some(guard),
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
                store = store_before_probe;
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
                active_guard.cloned(),
                arena,
            )? {
                store = next_store;
                boundaries = merge_boundaries(boundaries, recovered_boundaries);
                index += run_len;
                continue;
            }
        }

        (store, boundaries) = eval_statement_with_recovery(
            module,
            store,
            boundaries,
            &statements[index],
            arena,
            candidates,
            active_guard,
        )?;
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
        || !statements.len().is_multiple_of(trip_count)
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
            Factor::HierVariable(_) => false,
            Factor::Value(_) => true,
            Factor::FunctionCall(call) if call.outputs.is_empty() => call
                .inputs
                .values()
                .all(|input| collect_expression_variables(input, out)),
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

#[cfg(test)]
mod tests;
