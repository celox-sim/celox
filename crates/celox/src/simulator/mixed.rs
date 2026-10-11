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

pub(super) fn component_names(
    sources: &[(&str, &std::path::Path)],
) -> Result<fxhash::FxHashSet<String>, ParserError> {
    let mut names =
        celox_frontend_sv::component_names(sources).map_err(crate::parser::sv_analysis_error)?;
    names.extend(
        veryl_analyzer::symbol_table::get_all()
            .iter()
            .map(|symbol| symbol.token.text.to_string()),
    );
    Ok(names)
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
        sv::ConstExpr::Function { name, args, .. } => {
            if !name.starts_with('$') {
                return Err(unsupported(format!(
                    "header-dependent SV constant function `{name}` without an expression body"
                )));
            }
            format!(
                "{name}({})",
                args.iter()
                    .map(const_expr)
                    .collect::<Result<Vec<_>, _>>()?
                    .join(", ")
            )
        }
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
        sv::ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } => {
            let expr = const_expr(expr)?;
            let msb = const_expr(msb)?;
            let lsb = const_expr(lsb)?;
            let width =
                format!("(if ({msb}) >= ({lsb}) ? ({msb}) - ({lsb}) + 1 : ({lsb}) - ({msb}) + 1)");
            let low = format!("(if ({msb}) >= ({lsb}) ? ({lsb}) : ({msb}))");
            // Veryl selects require an identifier. Shift an arbitrary expression
            // and truncate at a width-context boundary, then restore the IR sign.
            format!(
                "${}(($unsigned({expr}) >> {low}) as ({width}))",
                if *signed { "signed" } else { "unsigned" }
            )
        }
        sv::ConstExpr::Resize {
            expr,
            width,
            signed,
        } => {
            if !matches!(width, 8 | 16 | 32 | 64) {
                return Err(unsupported(format!("SV declaration cast to {width} bits")));
            }
            format!(
                "({} as {}{width})",
                const_expr(expr)?,
                if *signed { "i" } else { "u" }
            )
        }
    })
}

// Only expressions depending on an overridable interface parameter must
// survive in the foreign contract. SV package constants and independent
// constant functions have already been evaluated by their owning analyzer.
fn depends_on_header(expr: &sv::ConstExpr, signature: &InterfaceSignature) -> bool {
    fn function(
        name: &str,
        signature: &InterfaceSignature,
        visited: &mut fxhash::FxHashSet<String>,
    ) -> bool {
        if name.starts_with('$') {
            return false;
        }
        if !visited.insert(name.to_string()) {
            return false;
        }
        let Some(subroutine) = signature
            .module
            .declaration_subroutines()
            .iter()
            .find(|function| function.name == name)
        else {
            return true;
        };
        let mut dependent = subroutine
            .return_type
            .as_ref()
            .is_some_and(|ty| type_depends(ty, signature, visited))
            || subroutine
                .params
                .iter()
                .any(|param| type_depends(&param.r#type, signature, visited));
        for statement in &subroutine.body {
            statement.walk(&mut |statement| {
                use sv::StmtBase::*;
                match statement {
                    Assign { lhs, rhs, .. } => {
                        dependent |= destination(lhs, signature, visited)
                            || expression(rhs, signature, visited);
                    }
                    AssignConcat { parts, rhs, .. } => {
                        dependent |= parts.iter().any(|lhs| destination(lhs, signature, visited))
                            || expression(rhs, signature, visited);
                    }
                    Eval(rhs) => dependent |= expression(rhs, signature, visited),
                    Return(Some(expr))
                    | Local {
                        init: Some(expr), ..
                    } => dependent |= expression(expr, signature, visited),
                    If { condition, .. } => dependent |= expression(condition, signature, visited),
                    Case {
                        selector, items, ..
                    } => {
                        dependent |= expression(selector, signature, visited);
                        for item in items {
                            for label in &item.labels {
                                match label {
                                    sv::CaseLabel::Value(value) => {
                                        dependent |= expression(value, signature, visited)
                                    }
                                    sv::CaseLabel::Range { low, high } => {
                                        dependent |= expression(low, signature, visited)
                                            || expression(high, signature, visited)
                                    }
                                }
                            }
                        }
                    }
                    Loop {
                        condition, kind, ..
                    } => {
                        if let Some(condition) = condition {
                            dependent |= expression(condition, signature, visited);
                        }
                        if let sv::LoopKind::Repeat(count) = kind {
                            dependent |= expression(count, signature, visited);
                        }
                    }
                    Call { name, args } => {
                        dependent |= function(name, signature, visited);
                        dependent |= args
                            .iter()
                            .flatten()
                            .any(|arg| expression(arg, signature, visited));
                    }
                    _ => {}
                }
            });
        }
        dependent
    }
    fn type_depends(
        ty: &sv::Type,
        signature: &InterfaceSignature,
        visited: &mut fxhash::FxHashSet<String>,
    ) -> bool {
        ty.packed_ranges().iter().any(|range| {
            visit(range.left(), signature, visited) || visit(range.right(), signature, visited)
        }) || ty.unpacked_ranges().iter().any(|range| {
            visit(range.left(), signature, visited) || visit(range.right(), signature, visited)
        })
    }
    fn destination(
        lhs: &sv::LValue,
        signature: &InterfaceSignature,
        visited: &mut fxhash::FxHashSet<String>,
    ) -> bool {
        match lhs {
            sv::LValue::Ident(_) => false,
            sv::LValue::Select {
                msb,
                lsb,
                array_slice_width,
                ..
            } => {
                visit(msb, signature, visited)
                    || visit(lsb, signature, visited)
                    || array_slice_width
                        .as_ref()
                        .is_some_and(|width| visit(width, signature, visited))
            }
        }
    }
    fn expression(
        expr: &sv::Expr,
        signature: &InterfaceSignature,
        visited: &mut fxhash::FxHashSet<String>,
    ) -> bool {
        use sv::Expr::*;
        match expr {
            Ident(name) => visit(&sv::ConstExpr::Ident(name.clone()), signature, visited),
            Literal(_) => false,
            Select { expr, msb, lsb, .. } => {
                expression(expr, signature, visited)
                    || visit(msb, signature, visited)
                    || visit(lsb, signature, visited)
            }
            Concat(parts) => parts
                .iter()
                .any(|part| expression(part, signature, visited)),
            RepeatConcat { count, parts } => {
                visit(count, signature, visited)
                    || parts
                        .iter()
                        .any(|part| expression(part, signature, visited))
            }
            Resize { expr, .. } | Unary { expr, .. } => expression(expr, signature, visited),
            Binary { left, right, .. } => {
                expression(left, signature, visited) || expression(right, signature, visited)
            }
            Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                expression(condition, signature, visited)
                    || expression(then_expr, signature, visited)
                    || expression(else_expr, signature, visited)
            }
            Call { name, args } => {
                function(name, signature, visited)
                    || args.iter().any(|arg| expression(arg, signature, visited))
            }
            Inside { expr, items } => {
                expression(expr, signature, visited)
                    || items.iter().any(|item| {
                        item.exprs()
                            .iter()
                            .any(|expr| expression(expr, signature, visited))
                    })
            }
        }
    }
    fn visit(
        expr: &sv::ConstExpr,
        signature: &InterfaceSignature,
        visited: &mut fxhash::FxHashSet<String>,
    ) -> bool {
        use sv::ConstExpr::*;
        match expr {
            Ident(name) => {
                if signature
                    .parameters
                    .iter()
                    .any(|parameter| name == &format!("h${parameter}"))
                {
                    return true;
                }
                if !visited.insert(name.clone()) {
                    return false;
                }
                signature
                    .module
                    .parameters()
                    .iter()
                    .find(|parameter| parameter.name() == name)
                    .and_then(|parameter| parameter.declaration_value())
                    .is_some_and(|value| visit(value, signature, visited))
            }
            Literal(_) => false,
            Binary { left, right, .. } => {
                visit(left, signature, visited) || visit(right, signature, visited)
            }
            Unary { expr, .. } | Resize { expr, .. } => visit(expr, signature, visited),
            Select { expr, bit } => {
                visit(expr, signature, visited) || visit(bit, signature, visited)
            }
            SelectRange { expr, msb, lsb, .. } => {
                visit(expr, signature, visited)
                    || visit(msb, signature, visited)
                    || visit(lsb, signature, visited)
            }
            Function { name, args, .. } => {
                function(name, signature, visited)
                    || args.iter().any(|arg| visit(arg, signature, visited))
            }
            Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                visit(condition, signature, visited)
                    || visit(then_expr, signature, visited)
                    || visit(else_expr, signature, visited)
            }
        }
    }
    visit(expr, signature, &mut fxhash::FxHashSet::default())
}

fn resolved_parameter(parameter: &sv::Parameter) -> Result<String, ParserError> {
    let value = parameter.resolved_value().ok_or_else(|| {
        unsupported(format!(
            "unresolved SV interface constant `{}`",
            parameter.name()
        ))
    })?;
    let width = parameter.resolved_width().unwrap_or(32);
    let signed = if parameter.resolved_signed().unwrap_or(false) {
        "s"
    } else {
        ""
    };
    let mask = if width < 128 {
        (1u128 << width) - 1
    } else {
        u128::MAX
    };
    Ok(format!("{width}'{signed}h{:x}", value as u128 & mask))
}

fn contract_expr(
    expr: &sv::ConstExpr,
    signature: &InterfaceSignature,
) -> Result<String, ParserError> {
    let mut constants = fxhash::FxHashMap::default();
    let mut types = fxhash::FxHashMap::default();
    for parameter in signature
        .module
        .imported_parameters()
        .iter()
        .chain(signature.module.parameters())
    {
        if signature
            .parameters
            .iter()
            .any(|name| parameter.name() == format!("h${name}"))
        {
            continue;
        }
        if let Some(value) = parameter.resolved_value() {
            // Body constants depending on the header retain their declaration
            // and symbolic reference so overrides still reach member widths.
            if parameter
                .declaration_value()
                .is_some_and(|value| depends_on_header(value, signature))
            {
                continue;
            }
            constants.insert(parameter.name().to_string(), value);
            types.insert(
                parameter.name().to_string(),
                (
                    parameter.resolved_width().unwrap_or(32),
                    parameter.resolved_signed().unwrap_or(false),
                ),
            );
        }
    }
    let expr = celox_frontend_sv::analyzer_typecheck::substitute_typed_constants(
        expr.clone(),
        &constants,
        &types,
    );
    const_expr(&expr)
}

fn dimension(
    left: &sv::ConstExpr,
    right: &sv::ConstExpr,
    signature: &InterfaceSignature,
) -> Result<String, ParserError> {
    let left = contract_expr(left, signature)?;
    let right = contract_expr(right, signature)?;
    Ok(format!(
        "(if ({left}) >= ({right}) ? ({left}) - ({right}) + 1 : ({right}) - ({left}) + 1)"
    ))
}

fn veryl_type(ty: &sv::Type, signature: &InterfaceSignature) -> Result<String, ParserError> {
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
                .map(|range| dimension(range.left(), range.right(), signature))
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
                .map(|range| dimension(range.left(), range.right(), signature))
                .collect::<Result<Vec<_>, _>>()?
                .join(", "),
        );
        text.push(']');
    }
    Ok(text)
}

fn parameter_type(
    parameter: &sv::Parameter,
    signature: &InterfaceSignature,
) -> Result<String, ParserError> {
    let width = if parameter.packed_ranges().is_empty() {
        parameter.resolved_width().unwrap_or(32).to_string()
    } else {
        parameter
            .packed_ranges()
            .iter()
            .map(|range| dimension(range.left(), range.right(), signature))
            .collect::<Result<Vec<_>, _>>()?
            .join(" * ")
    };
    Ok(format!(
        "{}logic<{width}>",
        if parameter.resolved_signed().unwrap_or(false) {
            "signed "
        } else {
            ""
        }
    ))
}

/// Register these declaration-only interfaces under the `$sv` namespace before
/// analyzing the authored Veryl sources, so foreign members are typed values.
pub(super) fn veryl_interface_contracts(
    signatures: &[InterfaceSignature],
) -> Result<String, ParserError> {
    let mut source = String::new();
    for signature in signatures {
        if signature.header_dependent_cast {
            return Err(unsupported("header-dependent SV declaration cast size"));
        }
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
                let value = parameter.declaration_value().ok_or_else(|| {
                    unsupported(format!("interface parameter `{name}` without a default"))
                })?;
                source.push_str(&format!(
                    "param {name}: {} = {},\n",
                    parameter_type(parameter, signature)?,
                    if depends_on_header(value, signature) {
                        contract_expr(value, signature)?
                    } else {
                        resolved_parameter(parameter)?
                    }
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
            if let Some(value) = parameter.declaration_value() {
                source.push_str(&format!(
                    "const {name}: {} = {};\n",
                    parameter_type(parameter, signature)?,
                    if depends_on_header(value, signature) {
                        contract_expr(value, signature)?
                    } else {
                        resolved_parameter(parameter)?
                    }
                ));
            }
        }
        for port in signature.module.ports() {
            let Some(name) = port.name().strip_prefix("h$") else {
                continue;
            };
            source.push_str(&format!(
                "var {name}: {};\n",
                veryl_type(port.r#type(), signature)?
            ));
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
pub(crate) fn attach_sv_interface_behavior(
    ir: &mut veryl_analyzer::ir::Ir,
    signatures: &[InterfaceSignature],
    context: &mut veryl_analyzer::Context,
    used_names: &mut fxhash::FxHashSet<String>,
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
        used_names: &mut fxhash::FxHashSet<String>,
        next_name: &mut usize,
    ) -> Result<(), ParserError> {
        let mut instance_names = module
            .declarations
            .iter()
            .filter_map(|declaration| {
                if let Declaration::Inst(instance) = declaration {
                    Some(instance.name.to_string())
                } else {
                    None
                }
            })
            .collect::<fxhash::FxHashSet<_>>();
        let mut next_instance = 0;
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
            let name = fresh_name("__celox_sv_interface", used_names, next_name);
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
            let mut internal_names = members
                .iter()
                .map(|variable| variable.path.0[1].to_string())
                .collect::<fxhash::FxHashSet<_>>();
            let mut next_internal = 0;
            let handle = fresh_name("h", &mut internal_names, &mut next_internal);
            let mut source = format!(
                "module {name}({});\n{}{} {parameter_text} {handle} {instance_dims} ();\n",
                ports.join(", "),
                signature.time_declarations,
                signature.name
            );
            let mut selects = String::new();
            for count in &dimensions {
                let variable = fresh_name("g", &mut internal_names, &mut next_internal);
                let block = fresh_name("elements", &mut internal_names, &mut next_internal);
                source.push_str(&format!(
                    "for(genvar {variable}=0; {variable}<{count}; {variable}++) begin: {block}\n"
                ));
                selects.push_str(&format!("[{variable}]"));
            }
            for variable in &members {
                let member = variable.path.0[1].to_string();
                if signature.driven.contains(&member) {
                    source.push_str(&format!(
                        "assign {member}{selects} = {handle}{selects}.{member};\n"
                    ));
                } else {
                    source.push_str(&format!(
                        "assign {handle}{selects}.{member} = {member}{selects};\n"
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
                    name: resource_table::insert_str(&fresh_name(
                        &format!("__sv_logic_{}", symbol.token.text),
                        &mut instance_names,
                        &mut next_instance,
                    )),
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
                attach(child, signatures, context, sources, used_names, next_name)?;
            }
        }
        Ok(())
    }
    let mut sources = Vec::new();
    let mut next_name = 0;
    for component in &mut ir.components {
        if let Component::Module(module) = component {
            attach(
                module,
                signatures,
                context,
                &mut sources,
                used_names,
                &mut next_name,
            )?;
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
    let authored =
        celox_frontend_sv::module_interfaces(sources).map_err(crate::parser::sv_analysis_error)?;
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
            && !authored.contains_key(&module.name.to_string())
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
                let symbol = symbol_table::resolve(&variable.token.beg)
                    .map_err(|_| unsupported("cannot resolve Veryl interface constant"))?
                    .found
                    .clone();
                let parameter = veryl_analyzer::symbol::Parameter {
                    name: symbol.token.text,
                    symbol: symbol.id,
                };
                let value = sv_value(
                    &parameter,
                    &property.parameters,
                    &interface.variables,
                    context,
                )?;
                source.push_str(&format!(
                    "localparam {} {} = {value};\n",
                    sv_type(&variable.r#type),
                    variable.path
                ));
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
        if !referenced.contains(&module.name.to_string())
            || authored.contains_key(&module.name.to_string())
        {
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

fn sv_value(
    parameter: &veryl_analyzer::symbol::Parameter,
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
    Ok(value)
}

fn sv_parameters(
    parameters: &[veryl_analyzer::symbol::Parameter],
    variables: &fxhash::FxHashMap<veryl_analyzer::ir::VarId, veryl_analyzer::ir::Variable>,
    context: &mut veryl_analyzer::Context,
) -> Result<String, ParserError> {
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
        let value = sv_value(parameter, parameters, variables, context)?;
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

pub(crate) fn specialize_sv_instances(
    ir: &mut veryl_analyzer::ir::Ir,
    sources: &[(&str, &std::path::Path)],
    context: &mut veryl_analyzer::Context,
    used_names: &mut fxhash::FxHashSet<String>,
) -> Result<Vec<celox_frontend_sv::SvModuleRequest>, ParserError> {
    use veryl_analyzer::{ir::*, symbol::SymbolKind, symbol_table};
    use veryl_parser::resource_table;
    let interfaces =
        celox_frontend_sv::module_interfaces(sources).map_err(crate::parser::sv_analysis_error)?;
    used_names.extend(
        ir.components
            .iter()
            .filter_map(|component| match component {
                Component::Module(module) => Some(module.name.to_string()),
                Component::SystemVerilog(module) => Some(module.name.to_string()),
                Component::Interface(_) => None,
            }),
    );
    fn numeric(
        value: &veryl_analyzer::value::Value,
    ) -> Result<celox_frontend_sv::SvParameterValue, ParserError> {
        if value.width() > 128 || value.mask_xz().as_ref() != &num_bigint::BigUint::default() {
            return Err(unsupported(
                "foreign numeric parameter wider than 128 bits or containing X/Z",
            ));
        }
        Ok(celox_frontend_sv::SvParameterValue {
            bits: value.payload_u128(),
            width: value.width(),
            signed: value.signed(),
        })
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
                used_names,
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
    parameters: &[(String, celox_frontend_sv::SvParameterValue)],
    used_names: &mut fxhash::FxHashSet<String>,
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
                veryl_analyzer::value::Value::from_u128(value.bits, 0, value.width, value.signed),
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
    let mut interface_values = fxhash::FxHashMap::default();
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
        symbol
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
        interface_values.insert((port, parameter), *value);
    }
    for (port, interface) in &mut modports {
        let symbol = symbol_table::get(interface.symbol).unwrap();
        let mut header = veryl_analyzer::Context::default();
        header.inherit(context);
        header.push_affiliation(veryl_analyzer::symbol::Affiliation::Interface);
        header.push_namespace(symbol.inner_namespace());
        interface.parameters.clear();
        let resolved = (|| -> Result<(), ParserError> {
            for parameter in symbol.kind.get_parameters() {
                let property = parameter.property();
                let token = property.token.into();
                let invalid = || {
                    unsupported(format!(
                        "cannot resolve interface parameter `{}`",
                        parameter.name
                    ))
                };
                let ty = property
                    .r#type
                    .to_ir_type(
                        &mut header,
                        veryl_analyzer::conv::utils::TypePosition::Variable,
                    )
                    .map_err(|_| invalid())?;
                let mut expression =
                    if let Some(value) = interface_values.get(&(*port, parameter.name)) {
                        let width = ty.total_width().ok_or_else(invalid)?;
                        let mut value = veryl_analyzer::value::Value::from_u128(
                            value.bits,
                            0,
                            value.width,
                            value.signed,
                        )
                        .expand(width, value.signed)
                        .into_owned();
                        value.trunc(width);
                        value.set_signed(ty.signed);
                        let mut comptime = Comptime::create_value(value.clone(), token);
                        comptime.r#type = ty.clone();
                        (comptime, Expression::create_value(value, token))
                    } else {
                        veryl_analyzer::conv::utils::eval_expr(
                            &mut header,
                            Some(ty.clone()),
                            property.value.as_ref().ok_or_else(invalid)?,
                            false,
                        )
                        .map_err(|_| invalid())?
                    };
                interface.add_parameter(parameter.name, expression.0.value.clone());
                let destination = AssignDestination {
                    id: VarId::default(),
                    path: VarPath::new(parameter.name),
                    index: VarIndex::default(),
                    select: VarSelect::default(),
                    comptime: Comptime::from_type(
                        ty,
                        veryl_analyzer::symbol::ClockDomain::None,
                        token,
                    ),
                    token,
                };
                veryl_analyzer::conv::utils::eval_const_assign(
                    &mut header,
                    (&property.kind).into(),
                    &destination,
                    &mut expression,
                )
                .map_err(|_| invalid())?;
            }
            Ok(())
        })();
        header.pop_namespace();
        context.inherit(&mut header);
        resolved?;
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
    specialized.name = resource_table::insert_str(&fresh_name(
        &format!("__celox_foreign_{name}"),
        used_names,
        &mut 0,
    ));
    let errors = context.drain_errors();
    if errors.iter().any(veryl_analyzer::AnalyzerError::is_error) {
        return Err(unsupported(format!(
            "native specialization of `{name}`: {errors:?}"
        )));
    }
    Ok(specialized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use veryl_analyzer::{
        Analyzer, Context,
        ir::{Component, Declaration, Ir},
    };
    use veryl_parser::{Parser, resource_table};

    #[test]
    fn dependent_statement_function_defaults_are_not_silently_folded() {
        let sv = "interface Bus #(parameter W = 8); function automatic integer twice(); integer result; result = 0; for (integer i = 0; i < W; i++) result += 2; return result; endfunction localparam N = twice(); logic [N-1:0] req; endinterface";
        let signatures =
            celox_frontend_sv::interface_signatures(&[(sv, std::path::Path::new("bus.sv"))])
                .unwrap();
        let error = veryl_interface_contracts(&signatures).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("header-dependent SV constant function"),
            "{error}"
        );
    }

    #[test]
    fn assignment_destinations_retain_constant_function_dependencies() {
        for assignment in ["result[W-1] = 1;", "{result[W-1], result[0]} = 2'b10;"] {
            let sv = format!(
                "interface Bus #(parameter W = 8); function automatic integer value(); integer result; result = 0; {assignment} return result; endfunction localparam N = value(); logic [7:0] req; endinterface"
            );
            let signatures = celox_frontend_sv::interface_signatures(&[(
                sv.as_str(),
                std::path::Path::new("bus.sv"),
            )])
            .unwrap_or_else(|error| panic!("{assignment}: {error}"));
            let error = veryl_interface_contracts(&signatures).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("header-dependent SV constant function"),
                "{assignment}: {error}"
            );
        }
    }

    #[test]
    fn dependent_cast_sizes_and_missing_symbolic_functions_are_rejected() {
        for source in [
            "interface Bus #(parameter W = 8); logic [W'(300)-1:0] req; endinterface",
            "interface Bus #(parameter W = 8); localparam C = W; logic [C'(300)-1:0] req; endinterface",
            "interface Bus #(parameter W = 8); typedef logic [W-1:0] word_t; logic [word_t'(300)-1:0] req; endinterface",
            "interface Bus #(parameter W = 8); function automatic integer count(); return W'(300); endfunction localparam N = count(); logic [N-1:0] req; endinterface",
            "interface Bus #(parameter W = 8); function automatic integer count(); logic [W-1:0] tmp; return $bits(tmp); endfunction localparam N = count(); logic [N-1:0] req; endinterface",
        ] {
            let signatures = celox_frontend_sv::interface_signatures(&[(
                source,
                std::path::Path::new("bus.sv"),
            )])
            .unwrap();
            let error = veryl_interface_contracts(&signatures).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("mixed-language interface contract"),
                "{error}"
            );
        }
    }

    #[test]
    fn independent_declaration_cast_sizes_remain_supported() {
        let source = "interface Bus #(parameter W = 8); localparam C = 16; logic [C'(300)-1:0] req; endinterface";
        let signatures =
            celox_frontend_sv::interface_signatures(&[(source, std::path::Path::new("bus.sv"))])
                .unwrap();
        assert!(!signatures[0].header_dependent_cast);
        veryl_interface_contracts(&signatures).unwrap();
    }

    #[test]
    fn constant_function_types_retain_header_dependencies() {
        // Formal copies and return values use their declared types (IEEE
        // 1800-2023 13.5.1 and 13.4.1), even when the body only returns a value.
        for (declarations, function, call) in [
            (
                "",
                "function automatic integer value(input logic [W-1:0] x); return x; endfunction",
                "value(16'h100)",
            ),
            (
                "",
                "function automatic logic [W-1:0] value(); return 16'h100; endfunction",
                "value()",
            ),
            (
                "localparam B = W; typedef logic [B-1:0] word_t;",
                "function automatic integer value(input word_t x); return x; endfunction",
                "value(16'h100)",
            ),
        ] {
            let source = format!(
                "interface Bus #(parameter W = 8); {declarations} {function} localparam N = {call}; logic [7:0] req; endinterface"
            );
            let signatures = celox_frontend_sv::interface_signatures(&[(
                source.as_str(),
                std::path::Path::new("bus.sv"),
            )])
            .unwrap();
            let error = veryl_interface_contracts(&signatures).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("header-dependent SV constant function"),
                "{function}: {error}"
            );
        }
        let source = "interface Bus #(parameter W = 8); localparam B = 16; typedef logic [B-1:0] word_t; function automatic word_t value(input word_t x); return x; endfunction localparam logic [15:0] N = value(16'h100); logic [7:0] req; endinterface";
        let signatures =
            celox_frontend_sv::interface_signatures(&[(source, std::path::Path::new("bus.sv"))])
                .unwrap();
        let contract = veryl_interface_contracts(&signatures).unwrap();
        assert!(
            contract
                .lines()
                .any(|line| line.starts_with("const N:") && line.ends_with(" = 16'h100;")),
            "{contract}"
        );
    }

    #[test]
    fn wrapper_instances_preserve_existing_ir_instance_names() {
        let sv = "interface Bus; logic [7:0] req, rsp; assign rsp = req + 1; endinterface module Child(input logic [7:0] a, output logic [7:0] y); assign y = a + 42; endmodule";
        let sources = [(sv, std::path::Path::new("bus.sv"))];
        let signatures = celox_frontend_sv::interface_signatures(&sources).unwrap();
        let contracts = veryl_interface_contracts(&signatures).unwrap();
        let contracts = Parser::parse(&contracts, &"contracts.veryl").unwrap();
        let authored = Parser::parse("module Top(a: input logic<8>, y: output logic<8>, authored: output logic<8>) { inst bus: $sv::Bus; assign bus.req = a; assign y = bus.rsp; inst child: $sv::Child(a, y: authored); }", &"top.veryl").unwrap();
        let metadata = veryl_metadata::Metadata::create_default("prj").unwrap();
        let analyzer = Analyzer::new(&metadata);
        assert!(analyzer.analyze_pass1("$sv", &contracts.veryl).is_empty());
        assert!(analyzer.analyze_pass1("prj", &authored.veryl).is_empty());
        assert!(Analyzer::analyze_post_pass1().is_empty());
        let mut context = Context::default();
        let mut ir = Ir::default();
        for parser in [&contracts, &authored] {
            let errors = analyzer.analyze_pass2(&parser.veryl, &mut context, Some(&mut ir));
            assert!(errors.iter().all(|error| !error.is_error()), "{errors:?}");
        }
        let Component::Module(module) = ir.components.iter_mut().find(|component|
            matches!(component, Component::Module(module) if module.name.to_string() == "Top")).unwrap() else { unreachable!() };
        let child = module
            .declarations
            .iter_mut()
            .find_map(|declaration| {
                if let Declaration::Inst(instance) = declaration {
                    Some(instance)
                } else {
                    None
                }
            })
            .unwrap();
        // Veryl reserves authored double-underscore identifiers, but internally
        // prepared IR can already contain helper instances with these names.
        child.name = resource_table::insert_str("__sv_logic_bus");
        let mut sibling = child.clone();
        sibling.name = resource_table::insert_str("__sv_logic_bus_0");
        module.declarations.push(Declaration::Inst(sibling));
        let mut names = component_names(&sources).unwrap();
        let generated =
            attach_sv_interface_behavior(&mut ir, &signatures, &mut context, &mut names).unwrap();
        assert_eq!(generated.len(), 1);
        let Component::Module(module) = ir.components.iter().find(|component|
            matches!(component, Component::Module(module) if module.name.to_string() == "Top")).unwrap() else { unreachable!() };
        let instances = module
            .declarations
            .iter()
            .filter_map(|declaration| {
                if let Declaration::Inst(instance) = declaration {
                    Some(instance.name.to_string())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(instances.len(), 3);
        assert_eq!(instances.iter().collect::<fxhash::FxHashSet<_>>().len(), 3);
        assert!(instances.iter().any(|name| name == "__sv_logic_bus"));
        assert!(instances.iter().any(|name| name == "__sv_logic_bus_0"));
    }
}
