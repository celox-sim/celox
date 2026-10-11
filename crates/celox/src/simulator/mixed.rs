//! Declaration contracts exchanged between the native HDL frontends.
//! Executable module bodies remain in their owning analyzer and lowerer.

use crate::ParserError;
use celox_frontend_sv::{InterfaceSignature, analyzer_ir as sv};

fn unsupported(detail: impl Into<String>) -> ParserError {
    ParserError::illegal_context("mixed-language interface contract", detail, None)
}

fn fresh_name(prefix: &str, used: &mut fxhash::FxHashSet<String>, next: &mut usize) -> String {
    loop {
        let index = *next;
        *next += 1;
        let name = format!("{prefix}_{index}");
        if used.insert(name.clone()) {
            return name;
        }
    }
}

pub(super) fn referenced_sv_components<'a>(
    sources: impl IntoIterator<Item = &'a veryl_parser::veryl_grammar_trait::Veryl>,
) -> fxhash::FxHashSet<String> {
    use veryl_parser::veryl_walker::VerylWalker;
    #[derive(Default)]
    struct References(fxhash::FxHashSet<String>);
    impl VerylWalker for References {
        fn scoped_identifier(
            &mut self,
            identifier: &veryl_parser::veryl_grammar_trait::ScopedIdentifier,
        ) {
            let path: veryl_analyzer::symbol_path::GenericSymbolPath = identifier.into();
            let path = path.generic_path();
            if path
                .as_slice()
                .first()
                .is_some_and(|name| name.to_string() == "$sv")
                && let Some(name) = path.as_slice().get(1)
            {
                self.0.insert(name.to_string());
            }
        }
    }
    let mut references = References::default();
    for source in sources {
        references.veryl(source);
    }
    references.0
}

fn binary(op: sv::BinaryOp) -> &'static str {
    use sv::BinaryOp::*;
    match op {
        Add => "+",
        Sub => "-",
        Mul => "*",
        Div => "/",
        Mod => "%",
        Pow => "**",
        Shl => "<<",
        Shr => ">>",
        Sar => ">>>",
        BitAnd => "&",
        BitOr => "|",
        BitXor => "^",
        LogicAnd => "&&",
        LogicOr => "||",
        Eq => "==",
        Ne => "!=",
        EqCase => "===",
        NeCase => "!==",
        EqWildcard => "==?",
        NeWildcard => "!=?",
        Lt => "<",
        Le => "<=",
        Gt => ">",
        Ge => ">=",
    }
}

fn const_expr(expr: &sv::ConstExpr) -> Result<String, ParserError> {
    Ok(match expr {
        sv::ConstExpr::Literal(value) => value.clone(),
        sv::ConstExpr::Ident(name) => name.strip_prefix("h$").unwrap_or(name).to_string(),
        sv::ConstExpr::Binary { left, op, right } => format!(
            "({} {} {})",
            const_expr(left)?,
            binary(*op),
            const_expr(right)?
        ),
        sv::ConstExpr::Unary { op, expr } => {
            let op = match op {
                sv::UnaryOp::Plus => "+",
                sv::UnaryOp::Minus => "-",
                sv::UnaryOp::BitNot => "~",
                sv::UnaryOp::LogicNot => "!",
                sv::UnaryOp::RedAnd => "&",
                sv::UnaryOp::RedOr => "|",
                sv::UnaryOp::RedXor => "^",
                sv::UnaryOp::ToTwoState => {
                    return Err(unsupported("two-state conversion in interface width"));
                }
            };
            format!("({op}{})", const_expr(expr)?)
        }
        sv::ConstExpr::Function { name, args, .. } => format!(
            "{name}({})",
            args.iter()
                .map(const_expr)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        sv::ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => format!(
            "(if {} ? {} : {})",
            const_expr(condition)?,
            const_expr(then_expr)?,
            const_expr(else_expr)?
        ),
        sv::ConstExpr::Select { expr, bit } => {
            format!("{}[{}]", const_expr(expr)?, const_expr(bit)?)
        }
        sv::ConstExpr::Resize { .. } => {
            return Err(unsupported("integral cast in SV interface declaration"));
        }
    })
}

fn dimension(left: &sv::ConstExpr, right: &sv::ConstExpr) -> Result<String, ParserError> {
    let left = const_expr(left)?;
    let right = const_expr(right)?;
    Ok(format!(
        "(if ({left}) >= ({right}) ? ({left}) - ({right}) + 1 : ({right}) - ({left}) + 1)"
    ))
}

fn veryl_type(ty: &sv::Type) -> Result<String, ParserError> {
    let mut text = if ty.is_signed() {
        "signed ".to_string()
    } else {
        String::new()
    };
    text.push_str(if ty.kind() == sv::TypeKind::Bit {
        "bit"
    } else {
        "logic"
    });
    if !ty.packed_ranges().is_empty() {
        text.push('<');
        text.push_str(
            &ty.packed_ranges()
                .iter()
                .map(|range| dimension(range.left(), range.right()))
                .collect::<Result<Vec<_>, _>>()?
                .join(", "),
        );
        text.push('>');
    }
    if !ty.unpacked_ranges().is_empty() {
        text.push('[');
        text.push_str(
            &ty.unpacked_ranges()
                .iter()
                .map(|range| dimension(range.left(), range.right()))
                .collect::<Result<Vec<_>, _>>()?
                .join(", "),
        );
        text.push(']');
    }
    Ok(text)
}

fn parameter_type(parameter: &sv::Parameter) -> String {
    format!(
        "{}logic<{}>",
        if parameter.resolved_signed().unwrap_or(false) {
            "signed "
        } else {
            ""
        },
        parameter.resolved_width().unwrap_or(32)
    )
}

/// Register these declaration-only interfaces under the `$sv` namespace before
/// analyzing the authored Veryl sources, so foreign members are typed values.
pub(super) fn veryl_interface_contracts(
    signatures: &[InterfaceSignature],
) -> Result<String, ParserError> {
    let mut source = String::new();
    for signature in signatures {
        source.push_str(&format!("interface {}", signature.name));
        if !signature.parameters.is_empty() {
            source.push_str(" #(\n");
            for name in &signature.parameters {
                let parameter = signature
                    .module
                    .parameters()
                    .iter()
                    .find(|parameter| parameter.name() == format!("h${name}"))
                    .ok_or_else(|| {
                        unsupported(format!(
                            "missing parameter `{name}` of `{}`",
                            signature.name
                        ))
                    })?;
                let value = parameter.value().ok_or_else(|| {
                    unsupported(format!("interface parameter `{name}` without a default"))
                })?;
                source.push_str(&format!(
                    "param {name}: {} = {},\n",
                    parameter_type(parameter),
                    const_expr(value)?
                ));
            }
            source.push(')');
        }
        source.push_str(" {\n");
        for parameter in signature.module.parameters() {
            let Some(name) = parameter.name().strip_prefix("h$") else {
                continue;
            };
            if signature
                .parameters
                .iter()
                .any(|parameter| parameter == name)
            {
                continue;
            }
            if let Some(value) = parameter.value() {
                source.push_str(&format!(
                    "const {name}: {} = {};\n",
                    parameter_type(parameter),
                    const_expr(value)?
                ));
            }
        }
        for port in signature.module.ports() {
            let Some(name) = port.name().strip_prefix("h$") else {
                continue;
            };
            source.push_str(&format!("var {name}: {};\n", veryl_type(port.r#type())?));
        }
        for (modport, members) in &signature.modports {
            source.push_str(&format!("modport {modport} {{\n"));
            for (name, direction) in members {
                if direction == "import" {
                    return Err(unsupported(format!(
                        "imported function `{name}` of SV interface `{}`",
                        signature.name
                    )));
                }
                source.push_str(&format!("{name}: {direction},\n"));
            }
            source.push_str("}\n");
        }
        source.push_str("}\n");
    }
    Ok(source)
}

fn sv_type(ty: &veryl_analyzer::ir::Type) -> String {
    use veryl_analyzer::ir::TypeKind;
    let base = if matches!(ty.kind, TypeKind::Bit) {
        "bit"
    } else {
        "logic"
    };
    let sign = if ty.signed { " signed" } else { "" };
    let dimensions = ty
        .width_expr()
        .iter()
        .map(|width| format!("[({})-1:0]", width.to_sv_expr()))
        .collect::<String>();
    let dimensions = if dimensions.is_empty() {
        ty.width()
            .iter()
            .filter_map(|width| width.map(|width| format!("[{width}-1:0]")))
            .collect()
    } else {
        dimensions
    };
    format!("{base}{sign} {dimensions}")
}

fn sv_array(ty: &veryl_analyzer::ir::Type) -> String {
    let dimensions = ty
        .array_expr()
        .iter()
        .map(|width| format!("[0:({})-1]", width.to_sv_expr()))
        .collect::<String>();
    if dimensions.is_empty() {
        ty.array
            .iter()
            .filter_map(|width| width.map(|width| format!("[0:{width}-1]")))
            .collect()
    } else {
        dimensions
    }
}

/// SV interfaces instantiated in Veryl keep their executable logic in an SV
/// child. Only the handle's typed member variables belong to the Veryl module.
pub(super) fn attach_sv_interface_behavior(
    ir: &mut veryl_analyzer::ir::Ir,
    signatures: &[InterfaceSignature],
    context: &mut veryl_analyzer::Context,
) -> Result<Vec<String>, ParserError> {
    use veryl_analyzer::{ir::*, symbol::SymbolKind, symbol_table};
    use veryl_parser::{
        resource_table,
        token_range::TokenRange,
        veryl_token::{Token, TokenSource},
    };
    fn attach(
        module: &mut Module,
        signatures: &[InterfaceSignature],
        context: &mut veryl_analyzer::Context,
        sources: &mut Vec<String>,
    ) -> Result<(), ParserError> {
        let mut groups = std::collections::BTreeMap::new();
        for variable in module.variables.values() {
            let Ok(symbol) = symbol_table::resolve(&variable.token.beg) else {
                continue;
            };
            let SymbolKind::Instance(property) = &symbol.found.kind else {
                continue;
            };
            let type_name = property
                .type_name
                .generic_path()
                .as_slice()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("::");
            let Some(signature) = signatures.iter().find(|signature| {
                type_name == format!("$sv::{}", signature.name) && signature.has_behavior
            }) else {
                continue;
            };
            if variable.path.0.len() != 2
                || matches!(variable.kind, VarKind::Param | VarKind::Const)
            {
                continue;
            }
            let entry = groups
                .entry(symbol.found.id)
                .or_insert_with(|| (symbol.found.clone(), signature, Vec::new()));
            entry.2.push(variable.clone());
        }
        for (_, (symbol, signature, mut members)) in groups {
            members.sort_by_key(|variable| variable.path.to_string());
            let SymbolKind::Instance(property) = &symbol.kind else {
                unreachable!();
            };
            let mut evaluation = veryl_analyzer::Context::default();
            evaluation.inherit(context);
            evaluation.variables = module.variables.clone();
            evaluation.functions = module.functions.clone();
            for variable in module.variables.values() {
                let mut comptime = Comptime::from_type(
                    variable.r#type.clone(),
                    veryl_analyzer::symbol::ClockDomain::None,
                    variable.token,
                );
                if matches!(variable.kind, VarKind::Param | VarKind::Const)
                    && let Some(value) = variable.value.first()
                {
                    comptime.value = ValueVariant::Numeric(value.clone());
                    comptime.is_const = true;
                }
                evaluation.insert_var_path_with_id(variable.path.clone(), variable.id, comptime);
            }
            let mut parameters = Vec::new();
            for (name, target) in &property.parameter_connects {
                let (comptime, _) = veryl_analyzer::conv::utils::eval_expr(
                    &mut evaluation,
                    None,
                    &target.expression,
                    false,
                )
                .map_err(|_| {
                    unsupported(format!(
                        "cannot resolve parameter `{}` of SV interface `{}`",
                        name.text, signature.name
                    ))
                })?;
                let value = comptime
                    .get_value()
                    .map_err(|_| unsupported("nonconstant SV interface parameter"))?;
                parameters.push(format!(".{}({value:x})", name.text));
            }
            parameters.sort();
            let mut dimensions = Vec::new();
            for dimension in &property.array {
                let (comptime, _) =
                    veryl_analyzer::conv::utils::eval_expr(&mut evaluation, None, dimension, false)
                        .map_err(|_| unsupported("cannot resolve SV interface instance array"))?;
                let count = comptime
                    .get_value()
                    .ok()
                    .and_then(|value| value.to_usize_saturating())
                    .ok_or_else(|| unsupported("nonconstant SV interface instance array"))?;
                if count == 0 {
                    return Err(unsupported("zero-sized SV interface instance array"));
                }
                dimensions.push(count);
            }
            let name = format!("__celox_sv_interface_{}", sources.len());
            let mut ports = Vec::new();
            let mut connects = Vec::new();
            let token = Token::new(&name, 0, 0, 0, 0, TokenSource::External);
            let token = TokenRange {
                beg: token,
                end: token,
            };
            for variable in &members {
                let member = variable.path.0[1].to_string();
                let direction = if signature.driven.contains(&member) {
                    "output"
                } else {
                    "input"
                };
                let mut ty = variable.r#type.clone();
                ty.set_concrete_width(ty.width().clone());
                let array = ty
                    .array
                    .iter()
                    .filter_map(|count| count.map(|count| format!("[0:{count}-1]")))
                    .collect::<String>();
                ports.push(format!("{direction} {} {member}{array}", sv_type(&ty)));
                connects.push(AssignDestination {
                    id: variable.id,
                    path: variable.path.clone(),
                    index: VarIndex::default(),
                    select: VarSelect::default(),
                    comptime: Comptime::from_type(
                        variable.r#type.clone(),
                        veryl_analyzer::symbol::ClockDomain::None,
                        token,
                    ),
                    token,
                });
            }
            let parameter_text = if parameters.is_empty() {
                String::new()
            } else {
                format!("#({})", parameters.join(", "))
            };
            let instance_dims = dimensions
                .iter()
                .map(|count| format!("[0:{}]", count - 1))
                .collect::<String>();
            let mut source = format!(
                "module {name}({});\n{} {parameter_text} h {instance_dims} ();\n",
                ports.join(", "),
                signature.name
            );
            let mut selects = String::new();
            for (index, count) in dimensions.iter().enumerate() {
                source.push_str(&format!(
                    "for(genvar g{index}=0; g{index}<{count}; g{index}++) begin: elements{index}\n"
                ));
                selects.push_str(&format!("[g{index}]"));
            }
            for variable in &members {
                let member = variable.path.0[1].to_string();
                if signature.driven.contains(&member) {
                    source.push_str(&format!(
                        "assign {member}{selects} = h{selects}.{member};\n"
                    ));
                } else {
                    source.push_str(&format!(
                        "assign h{selects}.{member} = {member}{selects};\n"
                    ));
                }
            }
            for _ in &dimensions {
                source.push_str("end\n");
            }
            source.push_str("endmodule\n");
            sources.push(source);
            module
                .declarations
                .push(Declaration::Inst(Box::new(InstDeclaration {
                    name: resource_table::insert_str(&format!("__sv_logic_{}", symbol.token.text)),
                    hierarchy: Vec::new(),
                    inputs: Vec::new(),
                    outputs: Vec::new(),
                    interface_bindings: Vec::new(),
                    component: std::sync::Arc::new(Component::SystemVerilog(SystemVerilog {
                        name: resource_table::insert_str(&name),
                        connects,
                    })),
                    token,
                })));
        }
        for declaration in &mut module.declarations {
            if let Declaration::Inst(instance) = declaration
                && let Component::Module(child) = std::sync::Arc::make_mut(&mut instance.component)
            {
                attach(child, signatures, context, sources)?;
            }
        }
        Ok(())
    }
    let mut sources = Vec::new();
    for component in &mut ir.components {
        if let Component::Module(module) = component {
            attach(module, signatures, context, &mut sources)?;
        }
    }
    Ok(sources)
}

/// Foreign declaration stubs supply types and positional port order to the SV
/// analyzer. Their bodies are never lowered; the native Veryl bodies replace
/// them when the common symbolic hierarchy is assembled.
pub(super) fn sv_declaration_contracts(
    ir: &veryl_analyzer::ir::Ir,
    context: &mut veryl_analyzer::Context,
    sources: &[(&str, &std::path::Path)],
    requests: &[celox_frontend_sv::SvModuleRequest],
) -> Result<String, ParserError> {
    use veryl_analyzer::{
        ir::{Component, Signature, TypeKind, VarKind},
        symbol::SymbolKind,
        symbol_table,
    };
    let mut source = String::new();
    let referenced = celox_frontend_sv::component_references(sources)
        .map_err(crate::parser::sv_analysis_error)?;
    let mut needed_interfaces = referenced.clone();
    needed_interfaces.extend(requests.iter().flat_map(|request| {
        request
            .interfaces
            .iter()
            .map(|(_, interface)| interface.clone())
    }));
    for component in &ir.components {
        if let Component::Module(module) = component
            && referenced.contains(&module.name.to_string())
        {
            for (ty, _) in module.port_types.values() {
                if let TypeKind::Modport(signature, _) = &ty.kind {
                    needed_interfaces.insert(
                        symbol_table::get(signature.symbol)
                            .unwrap()
                            .token
                            .text
                            .to_string(),
                    );
                }
            }
        }
    }
    let mut symbols = symbol_table::get_all();
    symbols.sort_by_key(|symbol| symbol.id);
    for symbol in &symbols {
        let SymbolKind::Interface(property) = &symbol.kind else {
            continue;
        };
        if symbol.namespace.to_string() == "$sv" || property.is_proto {
            continue;
        }
        if !needed_interfaces.contains(&symbol.token.text.to_string()) {
            continue;
        }
        let signature = Signature::new(symbol.id);
        let component =
            veryl_analyzer::conv::utils::get_component(context, &signature, property.range)
                .map_err(|_| {
                    unsupported(format!(
                        "cannot resolve Veryl interface `{}`",
                        symbol.token.text
                    ))
                })?;
        let Component::Interface(interface) = component.as_ref() else {
            continue;
        };
        let parameters = sv_parameters(&property.parameters, &interface.variables, context)?;
        source.push_str(&format!("interface {}{parameters};\n", symbol.token.text));
        let mut variables = interface.variables.values().collect::<Vec<_>>();
        variables.sort_by_key(|variable| variable.id);
        for variable in variables {
            if variable.kind == VarKind::Param {
                continue;
            }
            if variable.kind == VarKind::Const {
                if let Some(value) = variable.value.first() {
                    source.push_str(&format!("localparam {} = {value:x};\n", variable.path));
                }
                continue;
            }
            source.push_str(&format!(
                "{} {}{};\n",
                sv_type(&variable.r#type),
                variable.path,
                sv_array(&variable.r#type)
            ));
        }
        let mut modports = interface.modports.iter().collect::<Vec<_>>();
        modports.sort_by_key(|(name, _)| **name);
        for (name, members) in modports {
            let members = members
                .iter()
                .map(|(name, direction)| format!("{direction} {name}"))
                .collect::<Vec<_>>()
                .join(", ");
            source.push_str(&format!("modport {name}({members});\n"));
        }
        source.push_str("endinterface\n");
    }
    for component in &ir.components {
        let Component::Module(module) = component else {
            continue;
        };
        if !referenced.contains(&module.name.to_string()) {
            continue;
        }
        if module
            .variables
            .values()
            .any(|variable| variable.r#type.is_unknown())
        {
            continue;
        }
        let Some(symbol) = symbols.iter().find(|symbol| {
            symbol.token.text == module.name && matches!(symbol.kind, SymbolKind::Module(_))
        }) else {
            continue;
        };
        let SymbolKind::Module(property) = &symbol.kind else {
            continue;
        };
        let mut ports = Vec::new();
        for port in &property.ports {
            let name = port.name();
            let path = veryl_analyzer::ir::VarPath::new(name);
            let Some((ty, _)) = module.port_types.get(&path) else {
                continue;
            };
            if let TypeKind::Modport(signature, modport) = &ty.kind {
                let interface = symbol_table::get(signature.symbol).unwrap();
                ports.push(format!(
                    "{}.{modport} {name}{}",
                    interface.token.text,
                    sv_array(ty)
                ));
            } else if let Some(id) = module.ports.get(&path) {
                let variable = &module.variables[id];
                let direction = match variable.kind {
                    VarKind::Input => "input",
                    VarKind::Output => "output",
                    VarKind::Inout => "inout",
                    _ => continue,
                };
                ports.push(format!(
                    "{direction} {} {name}{}",
                    sv_type(ty),
                    sv_array(ty)
                ));
            }
        }
        let parameters = sv_parameters(&property.parameters, &module.variables, context)?;
        source.push_str(&format!(
            "module {}{parameters}({}); endmodule\n",
            module.name,
            ports.join(", ")
        ));
    }
    Ok(source)
}

fn sv_parameters(
    parameters: &[veryl_analyzer::symbol::Parameter],
    variables: &fxhash::FxHashMap<veryl_analyzer::ir::VarId, veryl_analyzer::ir::Variable>,
    context: &mut veryl_analyzer::Context,
) -> Result<String, ParserError> {
    use veryl_analyzer::ir::{Expression, Factor, Op, SystemFunctionKind};
    use veryl_parser::veryl_walker::VerylWalker;
    struct Dependencies {
        parameters: fxhash::FxHashSet<veryl_analyzer::symbol::SymbolId>,
        visited: fxhash::FxHashSet<veryl_analyzer::symbol::SymbolId>,
        dependent: bool,
    }
    impl VerylWalker for Dependencies {
        fn scoped_identifier(
            &mut self,
            identifier: &veryl_parser::veryl_grammar_trait::ScopedIdentifier,
        ) {
            use veryl_parser::veryl_grammar_trait::ScopedIdentifierGroup;
            if let ScopedIdentifierGroup::IdentifierScopedIdentifierOpt(group) =
                identifier.scoped_identifier_group.as_ref()
                && let Some(arguments) = &group.scoped_identifier_opt
            {
                self.with_generic_argument(&arguments.with_generic_argument);
            }
            for segment in &identifier.scoped_identifier_list {
                if let Some(arguments) = &segment.scoped_identifier_opt0 {
                    self.with_generic_argument(&arguments.with_generic_argument);
                }
            }
            let Ok(symbol) = veryl_analyzer::symbol_table::resolve(identifier) else {
                return;
            };
            if self.parameters.contains(&symbol.found.id) {
                self.dependent = true;
            } else if self.visited.insert(symbol.found.id) {
                match &symbol.found.kind {
                    veryl_analyzer::symbol::SymbolKind::Parameter(property) => {
                        if let Some(value) = &property.value {
                            self.expression(value);
                        }
                    }
                    veryl_analyzer::symbol::SymbolKind::Function(property) => {
                        if let Some(definition) = property
                            .definition
                            .and_then(veryl_analyzer::definition_table::get)
                            && let veryl_analyzer::definition_table::Definition::Function(function) =
                                definition.as_ref()
                        {
                            self.function_declaration(function);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    fn render(
        expr: &Expression,
        variables: &fxhash::FxHashMap<veryl_analyzer::ir::VarId, veryl_analyzer::ir::Variable>,
    ) -> Result<String, ParserError> {
        Ok(match expr {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::Variable(id, index, select, _)
                    if index.0.is_empty() && select.is_empty() =>
                {
                    variables
                        .get(id)
                        .ok_or_else(|| unsupported("unresolved parameter default reference"))?
                        .path
                        .to_string()
                }
                Factor::Value(value) => format!(
                    "{:x}",
                    value
                        .get_value()
                        .map_err(|_| unsupported("nonnumeric parameter default"))?
                ),
                Factor::SystemFunctionCall(call) => match &call.kind {
                    SystemFunctionKind::Clog2(input) => {
                        format!("$clog2({})", render(&input.0, variables)?)
                    }
                    _ => return Err(unsupported("system function in foreign parameter default")),
                },
                _ => return Err(unsupported("expression in foreign parameter default")),
            },
            Expression::Unary(op, value, _) => format!("({op}{})", render(value, variables)?),
            Expression::Binary(left, op, right, _) if *op != Op::As => {
                let operator = match op {
                    Op::Less => "<".to_string(),
                    Op::Greater => ">".to_string(),
                    _ => op.to_string(),
                };
                format!(
                    "({} {operator} {})",
                    render(left, variables)?,
                    render(right, variables)?
                )
            }
            Expression::Ternary(condition, yes, no, _) => format!(
                "({} ? {} : {})",
                render(condition, variables)?,
                render(yes, variables)?,
                render(no, variables)?
            ),
            Expression::Concatenation(parts, _) => {
                let parts = parts
                    .iter()
                    .map(|(value, count)| {
                        let value = render(value, variables)?;
                        Ok(match count {
                            Some(count) => format!("{{{}{{{value}}}}}", render(count, variables)?),
                            None => value,
                        })
                    })
                    .collect::<Result<Vec<_>, ParserError>>()?;
                format!("{{{}}}", parts.join(", "))
            }
            _ => return Err(unsupported("expression in foreign parameter default")),
        })
    }
    let mut evaluation = veryl_analyzer::Context::default();
    evaluation.inherit(context);
    evaluation.variables = variables.clone();
    evaluation.disalbe_const_opt = true;
    for variable in variables.values() {
        let mut comptime = veryl_analyzer::ir::Comptime::from_type(
            variable.r#type.clone(),
            veryl_analyzer::symbol::ClockDomain::None,
            variable.token,
        );
        if let Some(value) = variable.value.first() {
            comptime.value = veryl_analyzer::ir::ValueVariant::Numeric(value.clone());
            comptime.is_const = true;
        }
        evaluation.insert_var_path_with_id(variable.path.clone(), variable.id, comptime);
    }
    let mut declarations = Vec::new();
    for parameter in parameters {
        let variable = variables
            .values()
            .find(|variable| variable.path.0.as_slice() == [parameter.name])
            .ok_or_else(|| {
                unsupported(format!(
                    "cannot resolve foreign parameter `{}`",
                    parameter.name
                ))
            })?;
        let symbol = veryl_analyzer::symbol_table::get(parameter.symbol).unwrap();
        let property = parameter.property();
        let value = property
            .value
            .as_ref()
            .ok_or_else(|| unsupported("foreign parameter without a default"))?;
        let mut dependencies = Dependencies {
            parameters: parameters
                .iter()
                .map(|parameter| parameter.symbol)
                .collect(),
            visited: Default::default(),
            dependent: false,
        };
        dependencies.expression(value);
        let value = if dependencies.dependent {
            evaluation.push_namespace(symbol.namespace);
            let expression = veryl_analyzer::conv::utils::eval_expr(
                &mut evaluation,
                Some(variable.r#type.clone()),
                value,
                false,
            )
            .map_err(|_| {
                unsupported(format!(
                    "cannot resolve default of foreign parameter `{}`",
                    parameter.name
                ))
            });
            evaluation.pop_namespace();
            let (_, expression) = expression?;
            render(&expression, &evaluation.variables)?
        } else {
            // Defaults independent of the overridable header retain the full
            // native evaluator, including constant functions and casts.
            let value = variable
                .value
                .first()
                .ok_or_else(|| unsupported("foreign parameter without a concrete default"))?;
            format!("{value:x}")
        };
        declarations.push(format!(
            "parameter {} {} = {value}",
            sv_type(&variable.r#type),
            parameter.name
        ));
    }
    Ok(if declarations.is_empty() {
        String::new()
    } else {
        format!("#({})", declarations.join(", "))
    })
}

pub(super) fn specialize_sv_instances(
    ir: &mut veryl_analyzer::ir::Ir,
    sources: &[(&str, &std::path::Path)],
    context: &mut veryl_analyzer::Context,
) -> Result<Vec<celox_frontend_sv::SvModuleRequest>, ParserError> {
    use veryl_analyzer::{ir::*, symbol::SymbolKind, symbol_table};
    use veryl_parser::resource_table;
    let interfaces =
        celox_frontend_sv::module_interfaces(sources).map_err(crate::parser::sv_analysis_error)?;
    let mut used_names = interfaces.keys().cloned().collect::<fxhash::FxHashSet<_>>();
    used_names.extend(
        symbol_table::get_all()
            .iter()
            .map(|symbol| symbol.token.text.to_string()),
    );
    used_names.extend(
        ir.components
            .iter()
            .filter_map(|component| match component {
                Component::Module(module) => Some(module.name.to_string()),
                Component::SystemVerilog(module) => Some(module.name.to_string()),
                Component::Interface(_) => None,
            }),
    );
    fn numeric(value: &veryl_analyzer::value::Value) -> Result<i128, ParserError> {
        if value.width() > 128
            || value.mask_xz().as_ref() != &num_bigint::BigUint::default()
            || (!value.signed() && value.payload_u128() > i128::MAX as u128)
        {
            return Err(unsupported(
                "foreign numeric parameter wider than 128 bits or containing X/Z",
            ));
        }
        let raw = value.payload_u128();
        if value.signed()
            && value.width() > 0
            && value.width() < 128
            && (raw >> (value.width() - 1)) & 1 != 0
        {
            Ok((raw | (!0u128 << value.width())) as i128)
        } else {
            Ok(raw as i128)
        }
    }
    fn walk(
        module: &mut Module,
        interfaces: &celox_frontend_sv::ModuleInterfaces,
        context: &mut veryl_analyzer::Context,
        requests: &mut Vec<celox_frontend_sv::SvModuleRequest>,
        used_names: &mut fxhash::FxHashSet<String>,
        next_alias: &mut usize,
    ) -> Result<(), ParserError> {
        let mut evaluation = veryl_analyzer::Context::default();
        evaluation.inherit(context);
        evaluation.variables = module.variables.clone();
        evaluation.functions = module.functions.clone();
        for variable in module.variables.values() {
            let mut comptime = Comptime::from_type(
                variable.r#type.clone(),
                veryl_analyzer::symbol::ClockDomain::None,
                variable.token,
            );
            if matches!(variable.kind, VarKind::Param | VarKind::Const)
                && let Some(value) = variable.value.first()
            {
                comptime.value = ValueVariant::Numeric(value.clone());
                comptime.is_const = true;
            }
            evaluation.insert_var_path_with_id(variable.path.clone(), variable.id, comptime);
        }
        for declaration in &mut module.declarations {
            let Declaration::Inst(instance) = declaration else {
                continue;
            };
            if let Component::Module(child) = std::sync::Arc::make_mut(&mut instance.component) {
                walk(child, interfaces, context, requests, used_names, next_alias)?;
                continue;
            }
            let Component::SystemVerilog(component) = instance.component.as_ref() else {
                continue;
            };
            if instance.token.beg.source == veryl_parser::veryl_token::TokenSource::External {
                continue;
            }
            let name = component.name.to_string();
            let Some(prototype) = interfaces.get(&name) else {
                continue;
            };
            let mut parameters = std::collections::BTreeMap::new();
            if let Ok(symbol) = symbol_table::resolve(&instance.token.beg)
                && let SymbolKind::Instance(property) = &symbol.found.kind
            {
                for (parameter, target) in &property.parameter_connects {
                    let (value, _) = veryl_analyzer::conv::utils::eval_expr(
                        &mut evaluation,
                        None,
                        &target.expression,
                        false,
                    )
                    .map_err(|_| {
                        unsupported(format!(
                            "cannot resolve parameter `{}` of `{name}`",
                            parameter.text
                        ))
                    })?;
                    parameters.insert(
                        parameter.text.to_string(),
                        numeric(
                            value
                                .get_value()
                                .map_err(|_| unsupported("nonconstant SV module parameter"))?,
                        )?,
                    );
                }
            }
            let (named, formals) = celox_frontend_veryl::external_port_formal_names(instance)?;
            let mut bindings = Vec::new();
            let mut offset = 0;
            for (position, formal) in formals.iter().enumerate() {
                let Some(connection) = component.connects.get(offset) else {
                    break;
                };
                let token = connection.token;
                let mut count = 1;
                while component
                    .connects
                    .get(offset + count)
                    .is_some_and(|next| next.token.beg.pos == token.beg.pos)
                {
                    count += 1;
                }
                let source = token.source().get_text();
                let span: miette::SourceSpan = (&token).into();
                let actual = source
                    .get(span.offset()..span.offset() + span.len())
                    .unwrap_or_default();
                if connection.path.0.len() > 1 && !actual.contains('.') {
                    let formal = if named {
                        formal.clone()
                    } else {
                        prototype
                            .ports
                            .get(position)
                            .cloned()
                            .ok_or_else(|| unsupported("too many external port associations"))?
                    };
                    let root = connection.path.0[0];
                    let variable = &module.variables[&connection.id];
                    let mut signature = if let Some((ty, _)) =
                        module.port_types.get(&VarPath::new(root))
                    {
                        let TypeKind::Modport(signature, _) = &ty.kind else {
                            offset += count;
                            continue;
                        };
                        module
                            .signature
                            .modport_signatures
                            .iter()
                            .find(|(port, _)| *port == root)
                            .map(|(_, signature)| signature.clone())
                            .unwrap_or_else(|| signature.as_ref().clone())
                    } else {
                        let symbol = symbol_table::resolve(&variable.token.beg)
                            .map_err(|_| unsupported("cannot resolve foreign interface handle"))?;
                        let SymbolKind::Instance(property) = &symbol.found.kind else {
                            offset += count;
                            continue;
                        };
                        let mut signature =
                            Signature::from_path(&mut evaluation, property.type_name.clone())
                                .ok_or_else(|| {
                                    unsupported("cannot resolve foreign interface type")
                                })?;
                        for (name, target) in &property.parameter_connects {
                            let (value, _) = veryl_analyzer::conv::utils::eval_expr(
                                &mut evaluation,
                                None,
                                &target.expression,
                                false,
                            )
                            .map_err(|_| {
                                unsupported("cannot resolve foreign interface parameter")
                            })?;
                            signature.add_parameter(name.text, value.value);
                        }
                        signature
                    };
                    signature.normalize();
                    let symbol = symbol_table::get(signature.symbol).unwrap();
                    bindings.push((formal.clone(), symbol.token.text.to_string()));
                    for (name, value) in &signature.parameters {
                        let ValueVariant::Numeric(value) = value else {
                            return Err(unsupported("foreign interface type parameter"));
                        };
                        parameters.insert(format!("{formal}${name}"), numeric(value)?);
                    }
                }
                offset += count;
            }
            if !parameters.is_empty() || !bindings.is_empty() {
                let alias = fresh_name("__celox_sv_instance", used_names, next_alias);
                requests.push(celox_frontend_sv::SvModuleRequest {
                    alias: alias.clone(),
                    module: name,
                    parameters: parameters.into_iter().collect(),
                    interfaces: bindings,
                });
                let Component::SystemVerilog(component) =
                    std::sync::Arc::make_mut(&mut instance.component)
                else {
                    unreachable!();
                };
                component.name = resource_table::insert_str(&alias);
            }
        }
        Ok(())
    }
    let mut requests = Vec::new();
    let mut next_alias = 0;
    for component in &mut ir.components {
        if let Component::Module(module) = component {
            walk(
                module,
                &interfaces,
                context,
                &mut requests,
                &mut used_names,
                &mut next_alias,
            )?;
        }
    }
    Ok(requests)
}

pub(crate) fn specialize_foreign_module(
    base: &veryl_analyzer::ir::Ir,
    context: &mut veryl_analyzer::Context,
    name: &str,
    parameters: &[(String, i128)],
    index: usize,
) -> Result<veryl_analyzer::ir::Module, ParserError> {
    use veryl_analyzer::{ir::*, symbol_table};
    use veryl_parser::resource_table;
    let mut ir = base.clone();
    let ordinary = parameters
        .iter()
        .filter(|(name, _)| !name.contains('$'))
        .map(|(name, value)| {
            (
                name.clone(),
                veryl_analyzer::value::Value::from_u128(*value as u128, 0, 128, *value < 0),
            )
        })
        .collect::<Vec<_>>();
    if !ordinary.is_empty() {
        super::builder::elaborate_parameterized_top_values(&mut ir, context, name, &ordinary)?;
    }
    let module = ir
        .components
        .iter()
        .find_map(|component| match component {
            Component::Module(module) if module.name.to_string() == name => Some(module),
            _ => None,
        })
        .ok_or_else(|| unsupported(format!("missing native module `{name}`")))?;
    let mut signature = module.signature.clone();
    let mut modports: fxhash::FxHashMap<_, Signature> = fxhash::FxHashMap::default();
    for (parameter, value) in parameters {
        let Some((port, parameter)) = parameter.split_once('$') else {
            continue;
        };
        let port = resource_table::insert_str(port);
        let Some((ty, _)) = module.port_types.get(&VarPath::new(port)) else {
            return Err(unsupported(format!(
                "unknown interface port `{port}` of `{name}`"
            )));
        };
        let TypeKind::Modport(interface, _) = &ty.kind else {
            return Err(unsupported(format!(
                "`{port}` is not a modport port of `{name}`"
            )));
        };
        let interface = modports
            .entry(port)
            .or_insert_with(|| interface.as_ref().clone());
        let parameter = resource_table::insert_str(parameter);
        let symbol = symbol_table::get(interface.symbol).unwrap();
        let property = symbol
            .kind
            .get_parameters()
            .iter()
            .find(|property| property.name == parameter)
            .ok_or_else(|| {
                unsupported(format!(
                    "unknown parameter `{parameter}` of interface `{}`",
                    symbol.token.text
                ))
            })?;
        let ty = property
            .property()
            .r#type
            .to_ir_type(context, veryl_analyzer::conv::utils::TypePosition::Variable)
            .map_err(|_| {
                unsupported(format!(
                    "cannot resolve type of interface parameter `{parameter}`"
                ))
            })?;
        let width = ty
            .total_width()
            .ok_or_else(|| unsupported("unknown interface parameter width"))?;
        let mut value = veryl_analyzer::value::Value::from_u128(*value as u128, 0, 128, *value < 0)
            .expand(width, *value < 0)
            .into_owned();
        value.trunc(width);
        value.set_signed(ty.signed);
        interface.parameters.retain(|(name, _)| *name != parameter);
        interface.add_parameter(parameter, ValueVariant::Numeric(value));
    }
    for (port, interface) in &modports {
        let symbol = symbol_table::get(interface.symbol).unwrap();
        let overrides = interface
            .parameters
            .iter()
            .filter_map(|(name, value)| {
                let ValueVariant::Numeric(value) = value else {
                    return None;
                };
                Some((
                    VarPath::new(*name),
                    (
                        Comptime::create_value(value.clone(), module.token),
                        Expression::create_value(value.clone(), module.token),
                    ),
                ))
            })
            .collect();
        context.push_override(symbol.inner_namespace(), overrides);
        let result = veryl_analyzer::conv::utils::get_component(context, interface, module.token);
        context.pop_override();
        result.map_err(|_| {
            unsupported(format!(
                "cannot specialize interface behind `{port}` of `{name}`"
            ))
        })?;
        signature.add_modport_signature(*port, interface.clone());
    }
    let mut specialized = if modports.is_empty() {
        module.clone()
    } else {
        let symbol = symbol_table::get(signature.symbol).unwrap();
        let overrides = signature
            .parameters
            .iter()
            .filter_map(|(name, value)| {
                let ValueVariant::Numeric(value) = value else {
                    return None;
                };
                Some((
                    VarPath::new(*name),
                    (
                        Comptime::create_value(value.clone(), module.token),
                        Expression::create_value(value.clone(), module.token),
                    ),
                ))
            })
            .collect();
        context.push_override(symbol.inner_namespace(), overrides);
        context.modport_signatures.push(modports);
        let result = veryl_analyzer::conv::utils::get_component(context, &signature, module.token);
        context.modport_signatures.pop();
        context.pop_override();
        let component =
            result.map_err(|_| unsupported(format!("cannot specialize native module `{name}`")))?;
        let Component::Module(module) = component.as_ref() else {
            unreachable!();
        };
        module.clone()
    };
    specialized.eval_assign(context);
    specialized.name = resource_table::insert_str(&format!("__celox_foreign_{name}_{index}"));
    let errors = context.drain_errors();
    if errors.iter().any(veryl_analyzer::AnalyzerError::is_error) {
        return Err(unsupported(format!(
            "native specialization of `{name}`: {errors:?}"
        )));
    }
    Ok(specialized)
}
