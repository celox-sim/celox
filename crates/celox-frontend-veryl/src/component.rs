use celox_design::InstanceId;
use celox_testbench::{
    ComponentConnection, ComponentParameterValue, SourceLocation as TestbenchSourceLocation,
    TestbenchComponent,
};
use veryl_analyzer::{
    ir::{Declaration, ExternalParamValue, Module, TypeKind},
    value::Value,
};
use veryl_parser::resource_table::{self, StrId};

use super::{
    VerylComponentBinding, VerylComponentConnectionBinding, VerylComponentEventBinding,
    VerylComponentInputBinding,
};
use crate::{HashMap, HashSet, InstancePath, ParserError};

fn string_of(id: StrId) -> String {
    resource_table::get_str_value(id).unwrap_or_default()
}

fn source_location(
    token: &veryl_parser::token_range::TokenRange,
) -> Option<TestbenchSourceLocation> {
    let file = token
        .beg
        .source
        .get_path()
        .and_then(resource_table::get_path_value)?;
    Some(TestbenchSourceLocation {
        file: file.to_string_lossy().into_owned(),
        line: token.beg.line,
        column: token.beg.column,
    })
}

fn component_parameter(value: &ExternalParamValue) -> ComponentParameterValue {
    match value {
        ExternalParamValue::Str(value) => ComponentParameterValue::String(value.clone()),
        ExternalParamValue::Value(value) => {
            let mut words = match value {
                Value::U64(value) => vec![value.payload],
                Value::BigUint(value) => value.payload().iter_u64_digits().collect(),
            };
            words.resize(value.width().div_ceil(64).max(1), 0);
            ComponentParameterValue::Bits {
                words,
                width: value.width() as u32,
            }
        }
    }
}

fn input_target(expression: &veryl_analyzer::ir::Expression) -> Option<VerylComponentInputBinding> {
    use veryl_analyzer::ir::{Expression, Factor, Op};
    match expression {
        Expression::Term(term) => match term.as_ref() {
            Factor::Variable(id, index, select, _) => Some(VerylComponentInputBinding::Root {
                id: *id,
                index: index.clone(),
                select: select.clone(),
            }),
            Factor::HierVariable(reference) => {
                Some(VerylComponentInputBinding::Hierarchical(reference.clone()))
            }
            _ => None,
        },
        Expression::Unary(Op::BitNot, inner, _) => input_target(inner),
        _ => None,
    }
}

fn event_target(expression: &veryl_analyzer::ir::Expression) -> Option<VerylComponentEventBinding> {
    use veryl_analyzer::ir::{Expression, Factor};
    let Expression::Term(term) = expression else {
        return None;
    };
    match term.as_ref() {
        Factor::Variable(id, _, _, _) => Some(VerylComponentEventBinding::Root(*id)),
        Factor::HierVariable(reference) => {
            Some(VerylComponentEventBinding::Hierarchical(reference.clone()))
        }
        _ => None,
    }
}

pub(crate) fn collect(
    module: &Module,
    parent_instance: InstanceId,
    parent_path: &InstancePath,
    instance_ids: &HashMap<InstancePath, InstanceId>,
    indexed_instances: &HashSet<InstanceId>,
    names: &mut HashSet<String>,
) -> Result<(Vec<TestbenchComponent>, Vec<VerylComponentBinding>), ParserError> {
    let mut components = Vec::new();
    let mut bindings = Vec::new();
    for declaration in &module.declarations {
        let Declaration::External(external) = declaration else {
            continue;
        };
        let mut path_prefix = Vec::with_capacity(parent_path.0.len());
        let prefix = parent_path
            .0
            .iter()
            .map(|(name, index)| {
                path_prefix.push((name.clone(), *index));
                if instance_ids
                    .get(&InstancePath(path_prefix.clone()))
                    .is_some_and(|id| indexed_instances.contains(id))
                {
                    format!("{name}[{index}]")
                } else {
                    name.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(".");
        let local_name = string_of(external.name);
        let instance = if prefix.is_empty() {
            local_name
        } else {
            format!("{prefix}.{local_name}")
        };
        if !names.insert(instance.clone()) {
            return Err(ParserError::illegal_context(
                "testbench component elaboration",
                format!("duplicate component instance `{instance}`"),
                Some(&external.token),
            ));
        }
        let connections = external
            .connects
            .iter()
            .map(|connection| ComponentConnection {
                port: string_of(connection.port),
                group: connection.group.map(string_of),
                member: connection.member.map(string_of),
                input: connection.input,
                has_output: connection.output.is_some(),
                is_clock: connection.is_clock,
                is_reset: connection.is_reset,
                width: connection.width,
            })
            .collect();
        let connection_bindings = external
            .connects
            .iter()
            .map(|connection| {
                let output = connection.output.clone();
                let input_target = connection
                    .input
                    .then(|| input_target(&connection.expr))
                    .flatten();
                let sync_reset = connection.is_reset
                    && matches!(
                        connection.expr.comptime().r#type.kind,
                        TypeKind::ResetSyncHigh | TypeKind::ResetSyncLow
                    );
                let event = if (connection.is_clock || connection.is_reset) && !sync_reset {
                    output
                        .as_ref()
                        .map(|output| VerylComponentEventBinding::Root(output.id))
                        .or_else(|| event_target(&connection.expr))
                } else {
                    None
                };
                VerylComponentConnectionBinding {
                    port: string_of(connection.port),
                    input: connection.input.then(|| connection.expr.clone()),
                    input_target,
                    output,
                    event,
                }
            })
            .collect();
        components.push(TestbenchComponent {
            instance: instance.clone(),
            component: string_of(external.component),
            params: external
                .params
                .iter()
                .map(|(name, value)| (string_of(*name), component_parameter(value)))
                .collect(),
            connections,
            is_var_form: external.is_var_form,
            source: source_location(&external.token),
        });
        bindings.push(VerylComponentBinding {
            instance,
            parent_instance,
            functions: module.functions.clone(),
            connections: connection_bindings,
        });
    }
    Ok((components, bindings))
}
