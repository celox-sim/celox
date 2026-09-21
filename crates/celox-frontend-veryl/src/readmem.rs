use std::{cmp::Reverse, collections::hash_map::Entry};

use celox_design::{InitialStateData, InitialStateValue, ModuleId};
use num_traits::ToPrimitive as _;
use veryl_analyzer::ir::{
    Declaration, HierVarRef, Module, Statement, SystemFunctionKind, SystemFunctionOutput,
};

use crate::{HashMap, LoweringPhase, ParserError, ScheduledRtl};

pub(crate) type PreparedReadmem = HashMap<
    veryl_parser::token_range::TokenRange,
    celox_testbench::SemanticStatement<celox_design::StateAddr>,
>;

pub(crate) fn prepare_testbench_memories(
    lookup: &crate::FrontendLookup,
    source: &crate::VerylTestbenchSource,
) -> Result<PreparedReadmem, ParserError> {
    let mut prepared = PreparedReadmem::default();
    if let Some(statements) = &source.initial_statements {
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
                    let instance = lookup
                        .root_instance_and_module()
                        .ok_or_else(|| {
                            ParserError::illegal_context(
                                "$readmemh destination",
                                "root instance was not found",
                                Some(&call.comptime.token),
                            )
                        })?
                        .0;
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

/// Resolve memory initialization against concrete instances, after the neutral
/// scheduler has assigned state identities to every child variable.
pub(crate) fn elaborate_hierarchical_initial_memories(
    modules: &HashMap<ModuleId, &Module>,
    scheduled: &mut ScheduledRtl,
) -> Result<(), ParserError> {
    let lookup = &scheduled.frontend_lookup;
    let root_module = lookup.root_instance_and_module().map(|(_, id)| id);
    let hierarchical_modules = modules
        .iter()
        .filter_map(|(&id, module)| {
            let has_memory = module.declarations.iter().any(|declaration| {
                matches!(declaration, Declaration::Initial(initial)
                    if has_hierarchical_readmem(&initial.statements))
            });
            (Some(id) != root_module && has_memory).then_some(id)
        })
        .collect::<crate::HashSet<_>>();
    if hierarchical_modules.is_empty() {
        return Ok(());
    }
    let mut instances = lookup
        .instance_ids
        .iter()
        .filter(|(_, id)| {
            lookup
                .instance_module
                .get(id)
                .is_some_and(|module| hierarchical_modules.contains(module))
        })
        .collect::<Vec<_>>();
    // A parent's initialization can overwrite its children's initial values.
    instances.sort_by_key(|(path, _)| (Reverse(path.0.len()), path.0.clone()));
    let mut prepared: HashMap<ModuleId, Vec<(HierVarRef, InitialStateData)>> = HashMap::default();
    for (_, &instance_id) in instances {
        let Some(&module_id) = lookup.instance_module.get(&instance_id) else {
            continue;
        };
        let Some(module) = modules.get(&module_id) else {
            continue;
        };
        if let Entry::Vacant(entry) = prepared.entry(module_id) {
            let mut values = Vec::new();
            for declaration in &module.declarations {
                let Declaration::Initial(initial) = declaration else {
                    continue;
                };
                if !has_hierarchical_readmem(&initial.statements) {
                    continue;
                }
                let mut context = veryl_analyzer::Context::default();
                context.variables = module.variables.clone();
                for statement in &initial.statements {
                    super::module::visit_initial_statement(
                        statement,
                        &mut context,
                        &mut |filename, output, _| {
                            let SystemFunctionOutput::Hier(reference) = output else {
                                return Ok(());
                            };
                            let (_, _, data) =
                                read_hierarchical_memory(filename, reference, lookup, instance_id)?;
                            values.push(((**reference).clone(), data));
                            Ok(())
                        },
                    )?;
                }
            }
            entry.insert(values);
        }
        for (reference, data) in &prepared[&module_id] {
            let (address, _) = super::testbench::resolve_hierarchical_reference_from(
                lookup,
                instance_id,
                reference,
            )?;
            scheduled.design.initial_state.push(InitialStateValue {
                address,
                data: data.clone(),
            });
        }
    }
    Ok(())
}

fn has_hierarchical_readmem(statements: &[Statement]) -> bool {
    statements.iter().any(|statement| match statement {
        Statement::SystemFunctionCall(call) => matches!(
            &call.kind,
            SystemFunctionKind::Readmemh(_, SystemFunctionOutput::Hier(_))
        ),
        Statement::If(statement) => {
            has_hierarchical_readmem(&statement.true_side)
                || has_hierarchical_readmem(&statement.false_side)
        }
        Statement::For(statement) => has_hierarchical_readmem(&statement.body),
        _ => false,
    })
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
        ParserError::unsupported(
            111,
            LoweringPhase::SimulatorParser,
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
