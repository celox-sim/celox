use std::collections::hash_map::Entry;

use celox_design::InitialStateData;
use num_traits::ToPrimitive as _;
use veryl_analyzer::ir::{
    ForRange, HierVarRef, Statement, SystemFunctionKind, SystemFunctionOutput,
};

use crate::{HashMap, ParserError};

pub(crate) type PreparedReadmem = HashMap<
    veryl_parser::token_range::TokenRange,
    celox_testbench::SemanticStatement<celox_design::StateAddr>,
>;

pub(crate) fn prepare_testbench_memories(
    lookup: &crate::FrontendLookup,
    source: &crate::VerylTestbenchSource,
) -> Result<PreparedReadmem, ParserError> {
    let mut prepared = PreparedReadmem::default();
    for statements in source.initial_blocks() {
        prepare_statements(
            statements,
            lookup,
            source,
            &mut crate::HashSet::default(),
            &mut prepared,
        )?;
    }
    Ok(prepared)
}

fn prepare_statements(
    statements: &[Statement],
    lookup: &crate::FrontendLookup,
    source: &crate::VerylTestbenchSource,
    active: &mut crate::HashSet<veryl_analyzer::ir::VarId>,
    prepared: &mut PreparedReadmem,
) -> Result<(), ParserError> {
    for statement in statements {
        match statement {
            Statement::SystemFunctionCall(call) => match &call.kind {
                SystemFunctionKind::Readmemh(filename, SystemFunctionOutput::Hier(reference)) => {
                    let Entry::Vacant(entry) = prepared.entry(call.comptime.token) else {
                        continue;
                    };
                    let instance = source.base_instance(lookup);
                    let (address, width, data) =
                        read_hierarchical_memory(filename, reference, lookup, instance)?;
                    let InitialStateData::Writes(writes) = data else {
                        unreachable!("read_memory_file always returns write runs");
                    };
                    entry.insert(celox_testbench::TestbenchStatement::WriteMemory {
                        signal: celox_testbench::SemanticSignal { address, width },
                        writes,
                    });
                }
                SystemFunctionKind::Finish => break,
                _ => {}
            },
            Statement::If(statement) => {
                let constant = crate::bitaccess::eval_constexpr(&statement.cond)
                    .and_then(|value| value.to_usize());
                if constant != Some(0) {
                    prepare_statements(&statement.true_side, lookup, source, active, prepared)?;
                }
                if constant.is_none_or(|value| value == 0) {
                    prepare_statements(&statement.false_side, lookup, source, active, prepared)?;
                }
            }
            Statement::IfReset(statement) => {
                prepare_statements(&statement.true_side, lookup, source, active, prepared)?;
                prepare_statements(&statement.false_side, lookup, source, active, prepared)?;
            }
            Statement::For(statement) => {
                // Only skip a proven empty range. Runtime bounds still need
                // their memory files prepared before testbench execution.
                if statically_empty_range(&statement.range) {
                    continue;
                }
                prepare_statements(&statement.body, lookup, source, active, prepared)?;
            }
            Statement::Case(statement) => {
                for arm in &statement.arms {
                    prepare_statements(&arm.body, lookup, source, active, prepared)?;
                }
                prepare_statements(&statement.default, lookup, source, active, prepared)?;
            }
            Statement::FunctionCall(call) if active.insert(call.id) => {
                if let Some(body) = source.functions.get(&call.id).and_then(|function| {
                    function.get_function(call.index.as_deref().unwrap_or(&[]))
                }) {
                    prepare_statements(&body.statements, lookup, source, active, prepared)?;
                }
                active.remove(&call.id);
            }
            Statement::Break => break,
            _ => {}
        }
    }
    Ok(())
}

fn statically_empty_range(range: &ForRange) -> bool {
    let (ForRange::Forward {
        start,
        end,
        inclusive,
        ..
    }
    | ForRange::Reverse {
        start,
        end,
        inclusive,
        ..
    }
    | ForRange::Stepped {
        start,
        end,
        inclusive,
        ..
    }) = range;
    let mut context = veryl_analyzer::Context::default();
    let (Some(start), Some(end)) = (start.eval_value(&mut context), end.eval_value(&mut context))
    else {
        return false;
    };
    // The runtime loop uses an SV int. Outside its nonnegative range,
    // usize ordering can disagree with the signed runtime comparison.
    if start > i32::MAX as usize || end > i32::MAX as usize {
        return false;
    }
    if *inclusive {
        start > end
    } else {
        start >= end
    }
}

fn read_hierarchical_memory(
    filename: &veryl_analyzer::ir::SystemFunctionInput,
    reference: &HierVarRef,
    lookup: &crate::FrontendLookup,
    instance_id: celox_design::InstanceId,
) -> Result<(celox_design::StateAddr, usize, InitialStateData), ParserError> {
    let (address, info) =
        super::testbench::resolve_hierarchical_reference_from(lookup, instance_id, reference)?;
    let depth = info
        .array_dims
        .iter()
        .try_fold(1usize, |depth, &dim| depth.checked_mul(dim));
    let invalid = |detail| {
        ParserError::illegal_context(
            "$readmemh destination",
            detail,
            Some(&reference.comptime.token),
        )
    };
    if !reference.index.0.is_empty() || !reference.select.is_empty() || reference.select.1.is_some()
    {
        return Err(invalid(
            "hierarchical destination must be a whole unpacked array variable",
        ));
    }
    let Some(depth) = depth.filter(|&depth| depth != 0 && !info.array_dims.is_empty()) else {
        return Err(invalid(
            "destination must be an unpacked array with a resolved size",
        ));
    };
    if info.width == 0 || !info.width.is_multiple_of(depth) {
        return Err(invalid("destination element width could not be resolved"));
    }
    let data = super::module::read_memory_file(
        filename,
        16,
        info.width / depth,
        0,
        depth,
        &reference.comptime.token,
    )?;
    Ok((address, info.width, data))
}
