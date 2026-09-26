//! Warn about statically overlapping destinations of distinct output arguments.
//!
//! Copy-out consists of blocking assignments (IEEE 1800-2023 4.9.7), but 13.5
//! does not order those assignments between formals. This is a portability
//! warning, not a rejection or a change to Celox's chosen execution order.
//! Runtime indices are deliberately not treated as proven aliases.
use veryl_analyzer::ir::{
    ArrayLiteralItem, AssignDestination, CasePattern, Component, Declaration, Expression, Factor,
    ForBound, ForRange, FunctionCall, Ir, Module, Statement, SystemFunctionCall,
    SystemFunctionKind, TbMethod, VarIndex, VarSelect,
};

use crate::{
    FrontendDiagnostic, HashSet,
    bitaccess::{eval_var_select, is_static_access},
};

pub fn check_function_output_aliases(ir: &Ir) -> Vec<FrontendDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut seen = HashSet::default();
    let mut visited = HashSet::default();
    let mut modules: Vec<_> = ir
        .components
        .iter()
        .filter_map(|component| match component {
            Component::Module(module) => Some(module),
            _ => None,
        })
        .collect();
    while let Some(module) = modules.pop() {
        if !visited.insert(std::ptr::from_ref(module)) {
            continue;
        }
        let mut checker = Checker {
            module,
            diagnostics: &mut diagnostics,
            seen: &mut seen,
        };
        for declaration in &module.declarations {
            match declaration {
                Declaration::Comb(x) => checker.statements(&x.statements),
                Declaration::Ff(x) => checker.statements(&x.statements),
                Declaration::Initial(x) => checker.statements(&x.statements),
                Declaration::Final(x) => checker.statements(&x.statements),
                Declaration::Inst(x) => {
                    for input in &x.inputs {
                        for expression in &input.exprs {
                            checker.expression(expression);
                        }
                    }
                    for output in &x.outputs {
                        checker.destinations(&output.dst);
                    }
                    if let Component::Module(child) = x.component.as_ref() {
                        modules.push(child);
                    }
                }
                Declaration::External(_) | Declaration::Unsupported(_) | Declaration::Null => {}
            }
        }
        for function in module.functions.values() {
            for body in &function.functions {
                checker.statements(&body.statements);
            }
        }
    }
    diagnostics
}

struct Checker<'a, 'b> {
    module: &'a Module,
    diagnostics: &'b mut Vec<FrontendDiagnostic>,
    seen: &'b mut HashSet<(String, usize, usize)>,
}

impl Checker<'_, '_> {
    fn call(&mut self, call: &FunctionCall) {
        let outputs: Vec<_> = call.outputs.iter().collect();
        'pairs: for (i, (left_arg, left)) in outputs.iter().enumerate() {
            for (right_arg, right) in outputs.iter().skip(i + 1) {
                // Flattened struct members of one formal belong to a single
                // copy-out; only compare different source-level arguments.
                if let Some(function) = self.module.functions.get(&call.id) {
                    let formal = |path| {
                        function
                            .args
                            .iter()
                            .find(|arg| arg.members.iter().any(|(member, _, _)| member == path))
                    };
                    if let (Some(a), Some(b)) = (formal(left_arg), formal(right_arg))
                        && a.name == b.name
                    {
                        continue;
                    }
                }
                for a in left.iter() {
                    for b in right.iter() {
                        if a.id != b.id
                            || !is_static_access(&a.index, &a.select)
                            || !is_static_access(&b.index, &b.select)
                        {
                            continue;
                        }
                        let (Ok(a_bits), Ok(b_bits)) = (
                            eval_var_select(self.module, a.id, &a.index, &a.select),
                            eval_var_select(self.module, b.id, &b.index, &b.select),
                        ) else {
                            continue;
                        };
                        let lsb = a_bits.lsb.max(b_bits.lsb);
                        let msb = a_bits.msb.min(b_bits.msb);
                        if lsb <= msb {
                            let token = &call.comptime.token;
                            let key = (
                                token.beg.source.to_string(),
                                token.beg.pos as usize,
                                token.end.pos as usize,
                            );
                            if self.seen.insert(key) {
                                self.diagnostics.push(
                                    FrontendDiagnostic::unspecified_output_copy_order(
                                        token,
                                        format!(
                                            "outputs `{left_arg}` and `{right_arg}` overlap on `{}[{msb}:{lsb}]`; the final value can depend on copy-out order",
                                            a.path
                                        ),
                                    ),
                                );
                            }
                            break 'pairs;
                        }
                    }
                }
            }
        }
        // Nested calls still need their own diagnostic, even if the enclosing
        // call already warned. Bodies are visited once at their declarations.
        for expression in call.inputs.values() {
            self.expression(expression);
        }
        for destinations in call.outputs.values() {
            self.destinations(destinations);
        }
    }

    fn destinations(&mut self, destinations: &[AssignDestination]) {
        for destination in destinations {
            self.select(&destination.index, &destination.select);
        }
    }

    fn select(&mut self, index: &VarIndex, select: &VarSelect) {
        for expression in index.0.iter().chain(&select.0) {
            self.expression(expression);
        }
        if let Some((_, expression)) = &select.1 {
            self.expression(expression);
        }
    }

    fn expression(&mut self, expression: &Expression) {
        match expression {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::FunctionCall(call) => self.call(call),
                Factor::SystemFunctionCall(call) => self.system_call(call),
                Factor::Variable(_, index, select, _) => self.select(index, select),
                Factor::HierVariable(x) => self.select(&x.index, &x.select),
                Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => {}
            },
            Expression::Unary(_, inner, _) => self.expression(inner),
            Expression::Binary(left, _, right, _) => {
                self.expression(left);
                self.expression(right);
            }
            Expression::Ternary(cond, left, right, _) => {
                self.expression(cond);
                self.expression(left);
                self.expression(right);
            }
            Expression::Concatenation(items, _) => {
                for (value, repeat) in items {
                    self.expression(value);
                    if let Some(repeat) = repeat {
                        self.expression(repeat);
                    }
                }
            }
            Expression::ArrayLiteral(items, _) => {
                for item in items {
                    match item {
                        ArrayLiteralItem::Value(value, repeat) => {
                            self.expression(value);
                            if let Some(repeat) = repeat {
                                self.expression(repeat);
                            }
                        }
                        ArrayLiteralItem::Defaul(value) => self.expression(value),
                    }
                }
            }
            Expression::StructConstructor(_, fields, _) => {
                for (_, value) in fields {
                    self.expression(value);
                }
            }
        }
    }

    fn system_call(&mut self, call: &SystemFunctionCall) {
        match &call.kind {
            SystemFunctionKind::Bits(x)
            | SystemFunctionKind::Size(x)
            | SystemFunctionKind::Clog2(x)
            | SystemFunctionKind::Onehot(x)
            | SystemFunctionKind::Signed(x)
            | SystemFunctionKind::Unsigned(x) => self.expression(&x.0),
            SystemFunctionKind::Readmemh(input, output) => {
                self.expression(&input.0);
                self.destinations(&output.0);
            }
            SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
                for arg in args {
                    self.expression(&arg.0);
                }
            }
            SystemFunctionKind::Assert { cond, args, .. } => {
                self.expression(&cond.0);
                for arg in args {
                    self.expression(&arg.0);
                }
            }
            SystemFunctionKind::Finish => {}
        }
    }

    fn statements(&mut self, statements: &[Statement]) {
        for statement in statements {
            match statement {
                Statement::Assign(x) => {
                    self.expression(&x.expr);
                    self.destinations(&x.dst);
                }
                Statement::FunctionCall(call) => self.call(call),
                Statement::SystemFunctionCall(call) => self.system_call(call),
                Statement::If(x) => {
                    self.expression(&x.cond);
                    self.statements(&x.true_side);
                    self.statements(&x.false_side);
                }
                Statement::IfReset(x) => {
                    self.statements(&x.true_side);
                    self.statements(&x.false_side);
                }
                Statement::Case(x) => {
                    self.expression(&x.case_target);
                    for arm in &x.arms {
                        for pattern in &arm.patterns {
                            match pattern {
                                CasePattern::Eq(value) => self.expression(value),
                                CasePattern::Range { lo, hi, .. } => {
                                    self.expression(lo);
                                    self.expression(hi);
                                }
                            }
                        }
                        self.statements(&arm.body);
                    }
                    self.statements(&x.default);
                }
                Statement::For(x) => {
                    let (ForRange::Forward { start, end, .. }
                    | ForRange::Reverse { start, end, .. }
                    | ForRange::Stepped { start, end, .. }) = &x.range;
                    for bound in [start, end] {
                        if let ForBound::Expression(value) = bound {
                            self.expression(value);
                        }
                    }
                    self.statements(&x.body);
                }
                Statement::TbMethodCall(x) => {
                    if let Some(dst) = &x.ret {
                        self.select(&dst.index, &dst.select);
                    }
                    match &x.method {
                        TbMethod::ClockNext { count, period } => {
                            for value in count.iter().chain(period.as_deref()) {
                                self.expression(value);
                            }
                        }
                        TbMethod::ResetAssert { duration, .. } => {
                            if let Some(value) = duration {
                                self.expression(value);
                            }
                        }
                        TbMethod::FileOpen { name, .. } => self.expression(&name.0),
                        TbMethod::FileWrite { args } | TbMethod::Component { args, .. } => {
                            for arg in args {
                                self.expression(&arg.0);
                            }
                        }
                        TbMethod::RandomSeed { value } => self.expression(value),
                        TbMethod::RandomGetRange { min, max, .. } => {
                            self.expression(min);
                            self.expression(max);
                        }
                        TbMethod::FileClose
                        | TbMethod::FileFlush
                        | TbMethod::RandomGet { .. }
                        | TbMethod::RandomGetSeed => {}
                    }
                }
                Statement::Break | Statement::Unsupported(_) | Statement::Null => {}
            }
        }
    }
}
