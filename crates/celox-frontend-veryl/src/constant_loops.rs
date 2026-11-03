//! Materialize bounded static procedures for Celox's lane and loop proofs.
//!
//! Shared Veryl IR retains `For` nodes. Celox needs distinct constant iterator
//! identities to prove recovered folds and concrete stores to reapply VPI force
//! between iterations. Runtime ranges and loops with a break remain intact.

use std::sync::Arc;

use veryl_analyzer::value::Value;
use veryl_analyzer::{Context, ir::*};
use veryl_parser::{resource_table, token_range::TokenRange};

use crate::HashMap;

const STATEMENT_LIMIT: usize = 4096;

pub fn lower_constant_loops(ir: &mut Ir) {
    let mut rewritten = HashMap::default();
    for component in &mut ir.components {
        if let Component::Module(module) = component {
            lower_module(module, &mut rewritten);
        }
    }
}

fn lower_module(module: &mut Module, rewritten: &mut HashMap<*const Component, Arc<Component>>) {
    let mut context = Context::default();
    context.variables = std::mem::take(&mut module.variables);
    context.functions = module.functions.clone();
    let mut next = context
        .variables
        .keys()
        .chain(module.functions.keys())
        .copied()
        .max()
        .unwrap_or_default();
    next.inc();
    for declaration in &mut module.declarations {
        match declaration {
            Declaration::Comb(x) => expand(&mut x.statements, &mut context, &mut next),
            Declaration::Ff(x) => expand(&mut x.statements, &mut context, &mut next),
            Declaration::Inst(x) => {
                let key = Arc::as_ptr(&x.component);
                if let Some(component) = rewritten.get(&key) {
                    x.component = component.clone();
                } else {
                    let mut component = (*x.component).clone();
                    if let Component::Module(child) = &mut component {
                        lower_module(child, rewritten);
                    }
                    let component = Arc::new(component);
                    rewritten.insert(key, component.clone());
                    x.component = component;
                }
            }
            _ => {}
        }
    }
    for function in module.functions.values_mut() {
        for body in &mut function.functions {
            expand(&mut body.statements, &mut context, &mut next);
        }
    }
    module.variables = context.variables;
}

fn token(statement: &Statement) -> Option<TokenRange> {
    match statement {
        Statement::Assign(x) => Some(x.token),
        Statement::If(x) => Some(x.token),
        Statement::IfReset(x) => Some(x.token),
        Statement::Case(x) => Some(x.token),
        Statement::For(x) => Some(x.token),
        Statement::FunctionCall(x) => Some(x.comptime.token),
        Statement::SystemFunctionCall(x) => Some(x.comptime.token),
        _ => None,
    }
}

fn contains_token(statements: &[Statement], needle: TokenRange) -> bool {
    statements.iter().any(|statement| {
        token(statement).is_some_and(|outer| {
            outer.beg.source == needle.beg.source
                && outer.beg.pos <= needle.beg.pos
                && needle.end.pos + needle.end.length <= outer.end.pos + outer.end.length
        }) || match statement {
            Statement::For(x) => contains_token(&x.body, needle),
            _ => false,
        }
    })
}

fn has_break(statements: &[Statement]) -> bool {
    statements.iter().any(|statement| match statement {
        Statement::Break | Statement::TbMethodCall(_) => true,
        Statement::If(x) => has_break(&x.true_side) || has_break(&x.false_side),
        Statement::IfReset(x) => has_break(&x.true_side) || has_break(&x.false_side),
        Statement::Case(x) => has_break(&x.default) || x.arms.iter().any(|x| has_break(&x.body)),
        _ => false,
    })
}

fn expand(statements: &mut Vec<Statement>, context: &mut Context, next: &mut VarId) {
    let mut expanded = Vec::new();
    for mut statement in std::mem::take(statements) {
        if let Statement::For(looped) = &statement {
            let (start, end) = looped.range.bounds();
            let constant = |bound: &ForBound| match bound {
                ForBound::Const(..) => true,
                ForBound::Expression(x) => x.comptime().is_const,
            };
            if constant(start)
                && constant(end)
                && !has_break(&looped.body)
                && let Some(iterations) = looped.range.eval_iter(context)
                && iterations.len().saturating_mul(looped.body.len().max(1)) <= STATEMENT_LIMIT
            {
                let original = context.variables[&looped.var_id].clone();
                let locals = context
                    .variables
                    .values()
                    .filter(|variable| {
                        variable.id != looped.var_id
                            && contains_token(&looped.body, variable.token)
                            && variable
                                .path
                                .0
                                .starts_with(&original.path.0[..original.path.0.len() - 1])
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                for iteration in iterations {
                    let mut remap = HashMap::default();
                    let label = resource_table::insert_str(&format!("[{iteration}]"));
                    for mut variable in
                        std::iter::once(original.clone()).chain(locals.iter().cloned())
                    {
                        let old = variable.id;
                        variable.id = *next;
                        next.inc();
                        let prefix = original.path.0.len() - 1;
                        variable
                            .path
                            .0
                            .insert(prefix.min(variable.path.0.len()), label);
                        if old == looped.var_id {
                            variable.kind = VarKind::Const;
                            variable.value = vec![Value::new(
                                iteration as u64,
                                looped.var_type.total_width().expect("analyzed loop width"),
                                looped.var_type.signed,
                            )];
                        }
                        remap.insert(old, variable.id);
                        context.variables.insert(variable.id, variable);
                    }
                    let mut body = looped.body.clone();
                    remap_statements(&mut body, &remap, context);
                    expand(&mut body, context, next);
                    expanded.extend(body);
                }
                continue;
            }
        }
        match &mut statement {
            Statement::If(x) => {
                expand(&mut x.true_side, context, next);
                expand(&mut x.false_side, context, next);
            }
            Statement::IfReset(x) => {
                expand(&mut x.true_side, context, next);
                expand(&mut x.false_side, context, next);
            }
            Statement::Case(x) => {
                for arm in &mut x.arms {
                    expand(&mut arm.body, context, next);
                }
                expand(&mut x.default, context, next);
            }
            // A retained loop may have runtime-dependent control. Its body
            // stays together, including nested loops and their break scopes.
            _ => {}
        }
        expanded.push(statement);
    }
    *statements = expanded;
}

fn selectors(
    index: &mut VarIndex,
    select: &mut VarSelect,
    map: &HashMap<VarId, VarId>,
    context: &mut Context,
) {
    for expr in index
        .expressions_mut()
        .chain(select.0.iter_mut())
        .chain(select.1.iter_mut().map(|(_, x)| x))
    {
        expression(expr, map, context);
    }
}

fn destination(dst: &mut AssignDestination, map: &HashMap<VarId, VarId>, context: &mut Context) {
    if let Some(id) = map.get(&dst.id) {
        dst.id = *id;
        dst.path = context.variables[id].path.clone();
    }
    selectors(&mut dst.index, &mut dst.select, map, context);
}

fn call(call: &mut FunctionCall, map: &HashMap<VarId, VarId>, context: &mut Context) {
    for input in call.inputs.values_mut() {
        expression(input, map, context);
    }
    for output in call.outputs.values_mut().flatten() {
        destination(output, map, context);
    }
}

fn system_call(call: &mut SystemFunctionCall, map: &HashMap<VarId, VarId>, context: &mut Context) {
    match &mut call.kind {
        SystemFunctionKind::Bits(x)
        | SystemFunctionKind::Clog2(x)
        | SystemFunctionKind::Onehot(x)
        | SystemFunctionKind::Signed(x)
        | SystemFunctionKind::Unsigned(x) => expression(&mut x.0, map, context),
        SystemFunctionKind::Size(x, y) => {
            expression(&mut x.0, map, context);
            if let Some(y) = y {
                expression(&mut y.0, map, context);
            }
        }
        SystemFunctionKind::Readmemh(x, y) => {
            expression(&mut x.0, map, context);
            if let SystemFunctionOutput::Local(outputs) = y {
                for output in outputs {
                    destination(output, map, context);
                }
            }
        }
        SystemFunctionKind::Display(xs) | SystemFunctionKind::Write(xs) => {
            for x in xs {
                expression(&mut x.0, map, context);
            }
        }
        SystemFunctionKind::Assert { cond, args, .. } => {
            expression(&mut cond.0, map, context);
            for x in args {
                expression(&mut x.0, map, context);
            }
        }
        SystemFunctionKind::Finish => {}
    }
}

fn expression(expr: &mut Expression, map: &HashMap<VarId, VarId>, context: &mut Context) {
    match expr {
        Expression::Term(factor) => match factor.as_mut() {
            Factor::Variable(id, index, select, comptime) => {
                if let Some(new) = map.get(id) {
                    *id = *new;
                    if context.variables[id].kind == VarKind::Const
                        && context.variables[id].value.len() == 1
                        && !context.variables[id].value[0].is_xz()
                    {
                        comptime.is_const = true;
                        comptime.value =
                            ValueVariant::Numeric(context.variables[id].value[0].clone());
                    }
                }
                selectors(index, select, map, context);
            }
            Factor::HierVariable(x) => selectors(&mut x.index, &mut x.select, map, context),
            Factor::FunctionCall(x) => call(x, map, context),
            Factor::SystemFunctionCall(x) => system_call(x, map, context),
            _ => {}
        },
        Expression::Unary(_, x, _) => expression(x, map, context),
        Expression::Binary(x, _, y, _) => {
            expression(x, map, context);
            expression(y, map, context);
        }
        Expression::Ternary(x, y, z, _) => {
            expression(x, map, context);
            expression(y, map, context);
            expression(z, map, context);
        }
        Expression::Concatenation(items, _) => {
            for (x, repeat) in items {
                expression(x, map, context);
                if let Some(x) = repeat {
                    expression(x, map, context);
                }
            }
        }
        Expression::ArrayLiteral(items, _) => {
            for item in items {
                match item {
                    ArrayLiteralItem::Value(x, repeat) => {
                        expression(x, map, context);
                        if let Some(x) = repeat {
                            expression(x, map, context);
                        }
                    }
                    ArrayLiteralItem::Defaul(x) => expression(x, map, context),
                }
            }
        }
        Expression::StructConstructor(_, fields, _) => {
            for (_, x) in fields {
                expression(x, map, context);
            }
        }
    }
    let width = expr.comptime().r#type.total_width();
    expr.comptime_mut().evaluated = false;
    expr.eval_comptime(context, width);
}

fn remap_statements(
    statements: &mut [Statement],
    map: &HashMap<VarId, VarId>,
    context: &mut Context,
) {
    for statement in statements {
        match statement {
            Statement::Assign(x) => {
                expression(&mut x.expr, map, context);
                for dst in &mut x.dst {
                    destination(dst, map, context);
                }
                if let Some(dst) = &mut x.hier_dst {
                    selectors(&mut dst.index, &mut dst.select, map, context);
                }
            }
            Statement::If(x) => {
                expression(&mut x.cond, map, context);
                remap_statements(&mut x.true_side, map, context);
                remap_statements(&mut x.false_side, map, context);
            }
            Statement::IfReset(x) => {
                remap_statements(&mut x.true_side, map, context);
                remap_statements(&mut x.false_side, map, context);
            }
            Statement::Case(x) => {
                expression(&mut x.case_target, map, context);
                for arm in &mut x.arms {
                    for pattern in &mut arm.patterns {
                        match pattern {
                            CasePattern::Eq(x) => expression(x, map, context),
                            CasePattern::Range { lo, hi, .. } => {
                                expression(lo, map, context);
                                expression(hi, map, context);
                            }
                        }
                    }
                    remap_statements(&mut arm.body, map, context);
                }
                remap_statements(&mut x.default, map, context);
            }
            Statement::For(x) => {
                if let Some(id) = map.get(&x.var_id) {
                    x.var_id = *id;
                }
                let (start, end) = x.range.bounds_mut();
                for bound in [start, end] {
                    if let ForBound::Expression(x) = bound {
                        expression(x, map, context);
                    }
                }
                remap_statements(&mut x.body, map, context);
            }
            Statement::FunctionCall(x) => call(x, map, context),
            Statement::SystemFunctionCall(x) => system_call(x, map, context),
            _ => {}
        }
    }
}
