//! Semantic analysis and elaboration entry points.

use std::collections::HashMap;

use crate::{
    AnalyzerError, ast, ir,
    symbol::{ModuleTable, ParameterTable, PortTable},
};

pub fn analyze_source(source: ast::Source) -> Result<ir::Ir, AnalyzerError> {
    let mut module_table = ModuleTable::default();
    let mut modules = Vec::new();
    for module in source.modules() {
        let id = module_table.insert(module)?;
        let mut constants = HashMap::new();
        let mut parameter_types = HashMap::new();
        let mut parameter_table = ParameterTable::default();
        let mut parameters = Vec::new();
        for parameter in module.parameters() {
            parameter_table.insert(module, parameter)?;
            let value: Option<ir::ConstExpr> = parameter.value().cloned().map(Into::into);
            let resolved_value = parameter.resolved_value(&constants, &parameter_types);
            if let Some(resolved_value) = resolved_value {
                constants.insert(parameter.name().to_string(), resolved_value);
            }
            let resolved_type = parameter.resolved_type(&parameter_types);
            if let Some(r#type) = resolved_type {
                parameter_types.insert(parameter.name().to_string(), r#type);
            }
            parameters.push(ir::Parameter::new(
                parameter.name().to_string(),
                value,
                resolved_value,
                resolved_type.map(|r#type| r#type.width),
                resolved_type.map(|r#type| r#type.signed),
                parameter.declared_width(),
                parameter.declared_signed(),
            ));
        }

        let mut port_table = PortTable::default();
        let mut ports = Vec::new();
        for port in module.ports() {
            port_table.insert(module, port)?;
            ports.push(ir::Port::new(
                port.name().to_string(),
                port.direction().into(),
                ir::Type::from_ast(port.r#type().clone(), &constants),
                port.is_net(),
            ));
        }
        let signals = module
            .signals()
            .iter()
            .map(|signal| {
                ir::Signal::new(
                    signal.name().to_string(),
                    ir::Type::from_ast(signal.r#type().clone(), &constants),
                    signal.is_net(),
                )
            })
            .collect();
        let instances = module
            .instances()
            .iter()
            .map(|instance| {
                ir::Instance::new(
                    instance.module_name().to_string(),
                    instance.name().to_string(),
                    instance.parameter_names().to_vec(),
                    instance
                        .parameter_overrides()
                        .iter()
                        .map(|parameter| {
                            ir::ParameterOverride::new(
                                parameter.name().to_string(),
                                parameter.value().cloned().map(Into::into),
                            )
                        })
                        .collect(),
                    instance.condition().cloned().map(Into::into),
                    instance.port_names().to_vec(),
                    instance
                        .port_connections()
                        .iter()
                        .map(|connection| {
                            ir::PortConnection::new(
                                connection.formal().to_string(),
                                connection.actual().to_string(),
                                connection.actual_expr().cloned().map(Into::into),
                            )
                        })
                        .collect(),
                )
            })
            .collect();
        let assignments = module
            .assignments()
            .iter()
            .cloned()
            .map(Into::into)
            .collect();
        let comb_processes = module
            .comb_processes()
            .iter()
            .cloned()
            .map(Into::into)
            .collect();
        let ff_processes = module
            .ff_processes()
            .iter()
            .cloned()
            .map(Into::into)
            .collect();
        modules.push(ir::Module::new(
            id,
            module.name().to_string(),
            parameters,
            ports,
            signals,
            instances,
            assignments,
            comb_processes,
            ff_processes,
        ));
    }

    Ok(ir::Ir::new(modules))
}
