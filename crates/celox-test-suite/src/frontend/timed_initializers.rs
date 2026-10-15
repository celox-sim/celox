//! Stage mutable module/generate declaration initializers before procedures.
use super::*;
use sv_parser::{NodeEvent, RefNode, SyntaxTree};

pub(super) fn span(tree: &SyntaxTree, node: RefNode<'_>) -> Option<(usize, usize)> {
    let origin = |offset| {
        tree.get_origin(&sv_parser::Locate {
            offset,
            len: 1,
            line: 0,
        })
    };
    let mut range: Option<(usize, usize)> = None;
    for child in node {
        if let RefNode::Locate(locate) = child
            && locate.len > 0
            && let Some((_, start)) = origin(locate.offset)
            && let Some((_, last)) = origin(locate.offset + locate.len - 1)
        {
            range = Some(match range {
                None => (start, last + 1),
                Some((low, high)) => (low.min(start), high.max(last + 1)),
            });
        }
    }
    range
}

type Calls = std::collections::BTreeSet<(String, Vec<bool>)>;

fn expression_calls(expr: &celox_sv_analyzer::ir::Expr, calls: &mut Calls) {
    use celox_sv_analyzer::ir::Expr;
    match expr {
        Expr::Call { name, args, .. } => {
            calls.insert((name.clone(), args.iter().map(Option::is_some).collect()));
            for arg in args.iter().flatten() {
                expression_calls(arg, calls);
            }
        }
        Expr::Select { expr, .. } | Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
            expression_calls(expr, calls)
        }
        Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => {
            for part in parts {
                expression_calls(part, calls);
            }
        }
        Expr::Binary { left, right, .. } => {
            expression_calls(left, calls);
            expression_calls(right, calls);
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            expression_calls(condition, calls);
            expression_calls(then_expr, calls);
            expression_calls(else_expr, calls);
        }
        Expr::Inside { expr, items } => {
            expression_calls(expr, calls);
            for item in items {
                for expr in item.exprs() {
                    expression_calls(expr, calls);
                }
            }
        }
        Expr::Ident(_) | Expr::Literal(_) => {}
    }
}

fn effectful_functions(
    ir: &celox_sv_analyzer::ir::Ir,
) -> std::collections::BTreeSet<(String, String)> {
    use celox_sv_analyzer::ir::Stmt;
    let mut functions = BTreeMap::new();
    for module in ir.modules() {
        for subroutine in module
            .declaration_subroutines()
            .iter()
            .chain(module.subroutines())
        {
            let mut local_names = subroutine
                .params
                .iter()
                .map(|p| p.name.clone())
                .collect::<std::collections::BTreeSet<_>>();
            local_names.insert(subroutine.name.clone());
            local_names.extend(subroutine.return_var.iter().cloned());
            for statement in &subroutine.body {
                statement.walk(&mut |stmt| {
                    if let Stmt::Local { name, .. } = stmt {
                        local_names.insert(name.clone());
                    }
                });
            }
            let mut effect = subroutine.params.iter().any(|p| p.direction.is_written());
            let mut calls = std::collections::BTreeSet::new();
            let defaults = subroutine
                .params
                .iter()
                .map(|param| {
                    let mut calls = Calls::new();
                    if let Some(default) = &param.default {
                        expression_calls(default, &mut calls);
                    }
                    calls
                })
                .collect();
            for statement in &subroutine.body {
                statement.walk(&mut |stmt| match stmt {
                    Stmt::SystemTask { .. } => effect = true,
                    Stmt::Assign { lhs, .. } => effect |= !local_names.contains(lhs.name()),
                    Stmt::AssignConcat { parts, .. } => {
                        effect |= parts.iter().any(|lhs| !local_names.contains(lhs.name()))
                    }
                    Stmt::Call { name, args, .. } => {
                        calls.insert((name.clone(), args.iter().map(Option::is_some).collect()));
                    }
                    _ => {}
                });
                let _ = statement.clone().map(
                    &mut |expr| {
                        expression_calls(&expr, &mut calls);
                        expr
                    },
                    &mut |lhs| lhs,
                );
            }
            functions
                .entry((module.name().to_owned(), subroutine.name.clone()))
                .or_insert((effect, calls, defaults));
        }
    }
    type Effects = BTreeMap<(String, String), (bool, Calls, Vec<Calls>)>;
    fn reaches_effect(
        functions: &Effects,
        module: &str,
        name: &str,
        provided: &[bool],
        visiting: &mut std::collections::BTreeSet<(String, String, Vec<bool>)>,
    ) -> bool {
        let key = (module.to_owned(), name.to_owned(), provided.to_vec());
        if !visiting.insert(key.clone()) {
            return false;
        }
        let result = functions
            .get(&(module.to_owned(), name.to_owned()))
            .is_some_and(|(effect, calls, defaults)| {
                *effect
                    || calls
                        .iter()
                        .chain(
                            defaults
                                .iter()
                                .enumerate()
                                .filter(|(index, _)| {
                                    !provided.get(*index).copied().unwrap_or(false)
                                })
                                .flat_map(|(_, calls)| calls),
                        )
                        .any(|(callee, supplied)| {
                            reaches_effect(functions, module, callee, supplied, visiting)
                        })
            });
        visiting.remove(&key);
        result
    }
    functions
        .keys()
        .filter(|(module, name)| {
            reaches_effect(&functions, module, name, &[], &mut Default::default())
        })
        .cloned()
        .collect()
}

// Ask the declaration analyzer whether a value can be elaborated, while
// leaving the simulator to evaluate the original expression. A fixed-width
// two-state parameter also classifies wide and X/Z constants without making
// their numerical value part of this rewrite.
fn elaboration_constant(
    source: &str,
    start: usize,
    end: usize,
    expression: &str,
    seeds: &[String],
) -> bool {
    let mut suffix = start;
    let (name, guard) = loop {
        let name = format!("__celox_initializer_constant_{suffix}");
        let guard = format!("{name}_range");
        if !sv_words(source)
            .iter()
            .any(|(_, _, word)| *word == name || *word == guard || *word == format!("{name}_value"))
        {
            break (name, guard);
        }
        suffix += 1;
    };
    let function = format!("{name}_value");
    let probe = format!(
        "function automatic bit [127:0] {function}();\n{}\nreturn ({expression}); endfunction\nlocalparam bit [127:0] {name} = {function}(); wire [{name}:{name}] {guard}='0;\n",
        seeds.join("\n")
    );
    let candidate = if timed_ff::single_generate_body(source, start) {
        format!(
            "{}begin\n{probe}{}\nend\n{}",
            &source[..start],
            &source[start..end],
            &source[end..]
        )
    } else {
        format!("{}{probe}{}", &source[..start], &source[start..])
    };
    celox_sv_analyzer::analyze_source(&candidate, Path::new("timed.sv")).is_ok_and(|ir| {
        // Inspect the probe's resolved calls in its declaring module and
        // generate scope. A constant evaluator may ignore side effects;
        // effectful bodies or omitted defaults still require staged execution.
        let effects = effectful_functions(&ir);
        ir.modules().iter().any(|module| {
            let effectful = effects.iter().any(|(owner, name)| {
                owner == module.name() && name.rsplit('.').next() == Some(function.as_str())
            });
            !effectful
                && module.signals().iter().any(|signal| {
                    signal.name().rsplit('.').next() == Some(guard.as_str())
                        && signal.r#type().resolved_width() == Some(1)
                })
        })
    })
}

pub(super) fn rewrites(
    source: &str,
    names: &TimedNames,
) -> Result<BTreeMap<usize, (usize, String)>> {
    let mut result = BTreeMap::new();
    // Recursive statement rewrites do not contain module declarations.
    if !sv_words(source)
        .iter()
        .any(|(_, _, word)| matches!(*word, "module" | "interface"))
    {
        return Ok(result);
    }
    let tree = celox_sv_analyzer::syntax::parse_source(source, Path::new("timed.sv"))?;
    let mut scopes = Vec::new();
    let mut records = Vec::new();
    let mut used_names = sv_words(source)
        .into_iter()
        .map(|(_, _, word)| word.to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    let mut groups = BTreeMap::<usize, (usize, String)>::new();
    let mut static_seeds = BTreeMap::<usize, Vec<(String, String)>>::new();
    let mut modules = 0usize;
    let mut procedures = 0usize;
    for event in (&tree).into_iter().event() {
        let (entering, node) = match event {
            NodeEvent::Enter(node) => (true, node),
            NodeEvent::Leave(node) => (false, node),
        };
        if matches!(
            node,
            RefNode::ModuleDeclaration(_)
                | RefNode::InterfaceDeclaration(_)
                | RefNode::GenerateBlock(_)
        ) {
            if entering {
                scopes.push(
                    span(&tree, node.clone())
                        .ok_or("missing initializer scope")?
                        .0,
                );
            } else {
                scopes.pop();
            }
        }
        if matches!(
            node,
            RefNode::ModuleDeclaration(_) | RefNode::InterfaceDeclaration(_)
        ) {
            if entering {
                modules += 1;
            } else {
                modules -= 1;
            }
        }
        if matches!(
            node,
            RefNode::InitialConstruct(_)
                | RefNode::AlwaysConstruct(_)
                | RefNode::FinalConstruct(_)
                | RefNode::FunctionDeclaration(_)
                | RefNode::TaskDeclaration(_)
                | RefNode::ClassDeclaration(_)
        ) {
            if entering {
                procedures += 1;
            } else {
                procedures -= 1;
            }
        }
        if !entering || modules == 0 || procedures != 0 {
            continue;
        }
        let RefNode::DataDeclaration(sv_parser::DataDeclaration::Variable(variable)) = node else {
            continue;
        };
        let is_const = variable.nodes.0.is_some();
        let (start, end) = span(&tree, node).ok_or("missing declaration source span")?;
        let mut removals = Vec::new();
        let mut assignments = String::new();
        let mut temporaries = String::new();
        let datatype = span(&tree, RefNode::DataTypeOrImplicit(&variable.nodes.3))
            .and_then(|(begin, end)| source.get(begin..end))
            .unwrap_or("logic");
        for declaration in variable.nodes.4.nodes.0.contents() {
            let sv_parser::VariableDeclAssignment::Variable(declaration) = declaration else {
                continue;
            };
            let Some((equal, expression)) = &declaration.nodes.2 else {
                continue;
            };
            let (name_start, name_end) =
                span(&tree, RefNode::VariableIdentifier(&declaration.nodes.0))
                    .ok_or("missing initializer name")?;
            let (equal_start, _) =
                span(&tree, RefNode::Symbol(equal)).ok_or("missing initializer assignment")?;
            let literal_expression = !RefNode::Expression(expression).into_iter().any(|node| {
                matches!(
                    node,
                    RefNode::SimpleIdentifier(_)
                        | RefNode::EscapedIdentifier(_)
                        | RefNode::SystemTfIdentifier(_)
                )
            });
            let (expr_start, expr_end) = span(&tree, RefNode::Expression(expression))
                .ok_or("missing initializer expression")?;
            let name = source
                .get(name_start..name_end)
                .ok_or("initializer name span exceeds source")?;
            let expression = source
                .get(expr_start..expr_end)
                .ok_or("initializer expression span exceeds source")?;
            let mut dimensions = String::new();
            for dimension in &declaration.nodes.1 {
                let (begin, end) = span(&tree, RefNode::VariableDimension(dimension))
                    .ok_or("missing initializer dimensions")?;
                dimensions.push_str(
                    source
                        .get(begin..end)
                        .ok_or("initializer dimensions exceed source")?,
                );
            }

            let mut known = Vec::<(String, String)>::new();
            for scope in &scopes {
                for (name, value) in static_seeds.get(scope).into_iter().flatten() {
                    known.retain(|(previous, _)| previous != name);
                    known.push((name.clone(), value.clone()));
                }
            }
            let seeds = known
                .into_iter()
                .map(|(_, declaration)| declaration)
                .collect::<Vec<_>>();
            // Keep constant initialization before FF listeners are armed;
            // moving an already-high clock into setup would invent an edge.
            if literal_expression || elaboration_constant(source, start, end, expression, &seeds) {
                let scope = *scopes
                    .last()
                    .ok_or("constant initializer outside module scope")?;
                static_seeds.entry(scope).or_default().push((
                    name.trim().to_owned(),
                    format!("localparam {datatype} {name}{dimensions} = {expression};"),
                ));
                continue;
            }
            if is_const {
                return Err(crate::script::sv::Unsupported(format!(
                    "dynamic const initializer `{}` cannot be staged after host setup",
                    name.trim()
                ))
                .into());
            }
            let mut suffix = equal_start;
            let temporary = loop {
                let temporary = format!("{}_initializer_value_{suffix}", names.package);
                if used_names.insert(temporary.clone()) {
                    break temporary;
                }
                suffix += 1;
            };
            temporaries.push_str(&format!(", {temporary}{dimensions}"));
            assignments.push_str(&format!("if (!{0}::finished) begin {temporary} = {expression}; if (!{0}::finished) {name} = {temporary}; end\n", names.package));
            removals.push((equal_start, expr_end));
        }
        if removals.is_empty() {
            continue;
        }
        let (semicolon, _) = span(&tree, RefNode::Symbol(&variable.nodes.5))
            .ok_or("missing declaration terminator")?;
        let mut declaration = source
            .get(start..semicolon)
            .ok_or("declaration span exceeds source")?
            .to_owned();
        for (begin, end) in removals.into_iter().rev() {
            declaration.replace_range(begin - start..end - start, "");
        }
        declaration.push_str(&temporaries);
        declaration.push_str(
            source
                .get(semicolon..end)
                .ok_or("declaration terminator exceeds source")?,
        );
        let scope = *scopes.last().ok_or("initializer outside module scope")?;
        let index = records.len();
        groups
            .entry(scope)
            .and_modify(|(last, body)| {
                *last = index;
                body.push_str(&assignments);
            })
            .or_insert((index, assignments));
        records.push((start, end, declaration));
    }
    // One activation per lexical scope preserves declaration order across
    // declarations as well as within a multi-variable declaration.
    for (index, (start, end, declaration)) in records.into_iter().enumerate() {
        let body = groups
            .values()
            .find(|(last, _)| *last == index)
            .map(|(_, body)| body.as_str());
        let replacement = if let Some(body) = body {
            format!(
                "{declaration}\ninitial begin wait ({}::initializing);\n{body}end\n",
                names.package
            )
        } else {
            declaration
        };
        let replacement = if timed_ff::single_generate_body(source, start) {
            format!("begin\n{replacement}end\n")
        } else {
            replacement
        };
        result.insert(start, (end, replacement));
    }
    Ok(result)
}
